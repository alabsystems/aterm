// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! MEMORY STARVATION, a health episode ([`Watch`]): the machine is pushing anonymous pages out
//! of RAM and reading them back, or its compressor nears the wall past which none can leave.
//!
//! Only readings that need no process-table read judge it (a Mac's `host_statistics64` and a few
//! sysctls, Linux's `/proc/meminfo`, `/proc/vmstat` and PSI file), since that read stalls first
//! under starvation (ATERM-KI-1116-2320: the sweep stalled in `KERN_PROC_ALL` at 23:12:22, four
//! minutes before WindowServer's first watchdog kill). Of those, the honest ones are:
//! - the headroom ([`crate::model::MemSample::headroom`]), what the kernel hands out without
//!   pushing anyone's page out: 5.8 GiB of 128 at 23:16:09, 0.12 GiB at 23:20:24;
//! - the VM's activity ([`crate::model::MemRates::push_bps`],
//!   [`crate::model::MemRates::refault_bps`], [`crate::model::MemRates::anon_back_bps`]): pages
//!   pushed out and pages that came back, 259 and 89 MiB/s from 23:16:09 to 23:20:24, 0 and
//!   under 0.01 MiB/s on a healthy host;
//! - the compressor's wall ([`crate::model::MemSample::wall`]): about 95 % of its segments at
//!   23:16 by an estimate from the processes' footprints, 100 % at the panic;
//! - the kernel's level computed from the same read ([`crate::model::MemSample::kept_permille`]),
//!   beside the level it reports ([`crate::model::MemSample::pressure`]), which its pageout path
//!   stops updating when it stalls.
//!
//! The rest lie for this question: `kern.memorystatus_level` read 38 % or more throughout; the
//! compressor's size and the swap in use are stocks that keep idle programs' pages long after the
//! machine recovers (a healthy host holds 1.5 GiB in its compressor); the dispatch source's
//! notice comes last, to one process at a time.
//!
//! An episode OPENS where the machine starves now ([`Look::starving`]) at a reading and at every
//! reading after it for [`OPEN_HOLD_NS`] (the windows of the readings after the first span it),
//! each judged on its own window's average: at a cadence of 5 s or less a burst that two windows
//! hold opens none; at the hidden deck's 10 s it is two readings in a row. It ENDS where the
//! machine has been quiet ([`Look::quiet`]: the activity low, and the machine off its edge, since
//! one frozen at its wall moves no page) over [`END_HOLD_NS`] with no break. A reading that cannot
//! judge (no rates yet, none of the activity counters read) moves neither. Each edge names the
//! largest holders by footprint and their tabs from the newest process sweep the vitals hold
//! ([`Watch::step`]), never a fresh read.

use crate::model::{MemView, Pressure, RowScope, Snapshot};

/// How long the machine must go on starving after a starving reading, over readings with no
/// break, before an episode opens: 10 s.
pub const OPEN_HOLD_NS: u64 = 10_000_000_000;
/// How long the machine must stay quiet, over readings with no break, before an episode ends: a
/// minute.
pub const END_HOLD_NS: u64 = 60_000_000_000;
/// The compressor's fill, per mille of its segment limit, at which its wall is near: 900. The
/// kernel calls the compressor short of space at 980 ([`WALL_CRITICAL_PERMILLE`]).
pub const WALL_NEAR_PERMILLE: u16 = 900;
/// The fill at which xnu calls the compressor short of space and forces its level critical:
/// 980. An open episode is never quiet there ([`Look::quiet`]); a wall of swapped segments under
/// it may be stale.
pub const WALL_CRITICAL_PERMILLE: u16 = 980;
/// The kernel's warn line on a Mac ([`crate::model::MemSample::kept_permille`] under it): 500.
pub const KEPT_WARN_PERMILLE: u16 = 500;
/// The kernel's critical line on a Mac over 3 GB ([`crate::model::MemSample::kept_permille`]
/// under it): 343.
pub const KEPT_CRITICAL_PERMILLE: u16 = 343;
/// The share of a window every non-idle task stalled on memory (PSI `full`) that starves: 5 %.
pub const STALL_FULL_STARVING: f64 = 0.05;
/// …and the share under which the stall is quiet: 1 %.
pub const STALL_FULL_QUIET: f64 = 0.01;
/// The rate at which anonymous memory moves, out or back, for the wall's and the kernel's rules,
/// bytes per second: 8 MiB/s (512 pages of 16 KiB a second), the same on every machine, since a
/// page that leaves or comes back costs one page whatever the RAM. A healthy host reads 0
/// compressions and 0.4 decompressions a second (6.4 KiB/s); the incident pushed 259 MiB/s.
/// Near the wall the compressor can take little more, so the push falls; a thirtieth of the
/// incident's still moves.
pub const ANON_MOVING_BPS: f64 = 8.0 * MIB;
/// The headroom at or under which a machine that moves no page is stalled, not quiet: a Mac's
/// free target at its cap (4000 pages of 16 KiB, 64 MiB). Under it the kernel's pageout daemon
/// runs on any machine whose pageout path is not stalled, so nothing moving there is that path
/// waiting, as at 2026-11-15's panic (0.02 GiB of headroom).
pub const FROZEN_HEADROOM: u64 = 64 << 20;
/// How many holders a record names.
pub const HOLDERS: usize = 3;

const MIB: f64 = 1_048_576.0;
const GIB: u64 = 1 << 30;

/// The rate at which pages pushed out of RAM are activity, bytes per second: a 2048th of the
/// machine's memory each second, and at least 32 MiB/s (64 MiB/s on a 128 GiB Mac, whose
/// incident pushed 259).
#[must_use]
pub fn push_floor_bps(ram: u64) -> f64 {
    (ram as f64 / 2048.0).max(32.0 * MIB)
}

/// The rate at which pages coming back are activity, bytes per second: an 8192nd of the
/// machine's memory each second, and at least 8 MiB/s (16 MiB/s on a 128 GiB Mac, whose incident
/// read back 89).
#[must_use]
pub fn refault_floor_bps(ram: u64) -> f64 {
    (ram as f64 / 8192.0).max(8.0 * MIB)
}

/// The headroom at or under which the kernel has no room left to give without pushing pages
/// out: a 16th of the machine's memory, and at least 1 GiB (8 GiB on a 128 GiB Mac, whose
/// incident read 5.8 at 23:16:09).
#[must_use]
pub fn reserve_bytes(ram: u64) -> u64 {
    (ram / 16).max(GIB)
}

/// Whether memory reading `m` keeps the reader reading memory on its own, where nothing asks a
/// reading sooner (the host's memory watch): its headroom at its reserve, the compressor's wall
/// near, the kernel's level at warn or worse (as it reports it, or its share under the warn
/// line), or tasks stalled on memory for [`STALL_FULL_QUIET`] of the window. Each machine that
/// starves reads one of these first ([`Look::starving`] holds only where one does); a machine
/// with room to hand out reads none.
#[must_use]
pub fn tight(m: &MemView) -> bool {
    let s = &m.sample;
    s.total > 0
        && (s.headroom.is_some_and(|h| h <= reserve_bytes(s.total))
            || s.wall.is_some_and(|w| permille(w) >= WALL_NEAR_PERMILLE)
            || matches!(s.pressure, Pressure::Warn | Pressure::Critical)
            || s.kept_permille.is_some_and(|k| k < KEPT_WARN_PERMILLE)
            || m.rates
                .and_then(|r| r.stall)
                .is_some_and(|(_, full)| full >= STALL_FULL_QUIET))
}

/// Whether reading `s` keeps the reader reading memory on its own ([`tight`]): an episode is
/// open, which only a reading can end, or its memory is tight.
#[must_use]
pub fn watching(s: &Snapshot) -> bool {
    s.starving.is_some() || s.mem.as_ref().is_some_and(tight)
}

/// A `(used, limit)` wall's fill, per mille of its limit (`u16::MAX` past it; 0 with no limit).
fn permille((used, limit): (u64, u64)) -> u16 {
    if limit == 0 {
        return 0;
    }
    u16::try_from(u128::from(used) * 1000 / u128::from(limit)).unwrap_or(u16::MAX)
}

/// Which rule found the machine starving.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// Pages are pushed out and read back, both at their floors, with the headroom at its
    /// reserve.
    Thrash,
    /// The compressor nears its wall ([`WALL_NEAR_PERMILLE`]) while anonymous pages move
    /// ([`ANON_MOVING_BPS`]).
    Wall,
    /// The kernel's level is critical, as it reports it or as its own test reads the queues
    /// ([`KEPT_CRITICAL_PERMILLE`]), while anonymous pages move ([`ANON_MOVING_BPS`]).
    Kernel,
    /// Every non-idle task stalled on memory for [`STALL_FULL_STARVING`] of the window (Linux's
    /// PSI).
    Stall,
}

impl Why {
    /// The word a record and `sys events` print (`why=`).
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Thrash => "thrash",
            Self::Wall => "wall",
            Self::Kernel => "kernel",
            Self::Stall => "stall",
        }
    }
}

/// What one reading says of the machine's memory, from readings that need no process table.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Look {
    /// Anonymous memory pushed out of RAM, bytes per second.
    pub push_bps: Option<f64>,
    /// Memory that came back, file-backed pages reactivated among it on a Mac, bytes per second.
    pub refault_bps: Option<f64>,
    /// Anonymous memory read back, from the compressor or from swap, bytes per second.
    pub anon_back_bps: Option<f64>,
    /// The headroom, bytes.
    pub headroom: Option<u64>,
    /// The machine's memory, bytes.
    pub ram: u64,
    /// The compressor's fill, per mille of its segment limit.
    pub wall_permille: Option<u16>,
    /// The kernel's pressure test's share ([`crate::model::MemSample::kept_permille`]).
    pub kept_permille: Option<u16>,
    /// The level the kernel reports.
    pub pressure: Pressure,
    /// The share of the window some task, and every non-idle task, stalled on memory.
    pub stall: Option<(f64, f64)>,
    /// Swap in use, bytes.
    pub swap_used: Option<u64>,
    /// Physical memory holding compressed pages, bytes.
    pub compressed: Option<u64>,
    /// The window the rates span, ns.
    pub window_ns: u64,
}

impl Look {
    /// The look `m` gives: `None` before its rates, or where it read none of the activity
    /// (pages pushed out, pages back, the stall), which leaves nothing to judge.
    #[must_use]
    pub fn of(m: &MemView) -> Option<Self> {
        let r = m.rates?;
        let finite = |v: Option<f64>| v.filter(|v| v.is_finite() && *v >= 0.0);
        let look = Self {
            push_bps: finite(r.push_bps),
            refault_bps: finite(r.refault_bps),
            anon_back_bps: finite(Some(r.anon_back_bps)),
            headroom: m.sample.headroom,
            ram: m.sample.total,
            wall_permille: m.sample.wall.filter(|w| w.1 > 0).map(permille),
            kept_permille: m.sample.kept_permille,
            pressure: m.sample.pressure,
            stall: r.stall,
            swap_used: m.sample.swap_used,
            compressed: m.sample.compressed,
            window_ns: r.window_ns,
        };
        (look.ram > 0
            && (look.push_bps.is_some() || look.refault_bps.is_some() || look.stall.is_some()))
        .then_some(look)
    }

    fn pushing(&self, share: f64) -> bool {
        self.push_bps
            .is_some_and(|b| b >= push_floor_bps(self.ram) * share)
    }

    fn refaulting(&self, share: f64) -> bool {
        self.refault_bps
            .is_some_and(|b| b >= refault_floor_bps(self.ram) * share)
    }

    /// Whether anonymous memory moves, out or back, at `share` of [`ANON_MOVING_BPS`]. File-backed
    /// pages that come back (a build reading its cache again) are no part of it.
    fn anon_moving(&self, share: f64) -> bool {
        let floor = ANON_MOVING_BPS * share;
        self.push_bps.is_some_and(|b| b >= floor) || self.anon_back_bps.is_some_and(|b| b >= floor)
    }

    fn wall_near(&self) -> bool {
        self.wall_permille.is_some_and(|w| w >= WALL_NEAR_PERMILLE)
    }

    /// Whether the kernel's level is critical: as it reports it, or as its own test reads this
    /// look (the share under [`KEPT_CRITICAL_PERMILLE`]).
    fn kernel_critical(&self) -> bool {
        self.pressure == Pressure::Critical
            || self
                .kept_permille
                .is_some_and(|k| k < KEPT_CRITICAL_PERMILLE)
    }

    /// Whether the machine stands at its edge, where a page that cannot move reads as nothing
    /// moving: the kernel's level critical ([`Self::kernel_critical`]), the compressor at
    /// [`WALL_CRITICAL_PERMILLE`], or the headroom at [`FROZEN_HEADROOM`].
    fn at_edge(&self) -> bool {
        self.kernel_critical()
            || self
                .wall_permille
                .is_some_and(|w| w >= WALL_CRITICAL_PERMILLE)
            || self.headroom.is_some_and(|h| h <= FROZEN_HEADROOM)
    }

    /// The rule this look starves by, the first that holds: pages pushed out and read back at
    /// their floors with the headroom at its reserve ([`Why::Thrash`]); the wall near while
    /// anonymous pages move ([`Why::Wall`]); the kernel critical while they move
    /// ([`Why::Kernel`]); every task stalled ([`Why::Stall`]). A headroom at its reserve alone, a
    /// stale compressor, stale swap, a wall of swapped segments with no anonymous page moving
    /// (file-backed pages coming back included), and the kernel's warn are no rule: each holds on
    /// a healthy machine.
    #[must_use]
    pub fn starving(&self) -> Option<Why> {
        let reserve = self.headroom.is_some_and(|h| h <= reserve_bytes(self.ram));
        if self.pushing(1.0) && self.refaulting(1.0) && reserve {
            return Some(Why::Thrash);
        }
        if self.anon_moving(1.0) && self.wall_near() {
            return Some(Why::Wall);
        }
        if self.anon_moving(1.0) && self.kernel_critical() {
            return Some(Why::Kernel);
        }
        if self
            .stall
            .is_some_and(|(_, full)| full >= STALL_FULL_STARVING)
        {
            return Some(Why::Stall);
        }
        None
    }

    /// Whether the machine is quiet: pages pushed out and back each under half its floor,
    /// anonymous pages under half theirs while the wall is near, the stall under
    /// [`STALL_FULL_QUIET`], and the machine off its edge (the kernel's level critical, the
    /// compressor at [`WALL_CRITICAL_PERMILLE`], the headroom at [`FROZEN_HEADROOM`]): a machine
    /// frozen at its wall can neither push a page nor read one back, so its counters stand
    /// still. The compressor's size and the swap are not asked: after a starving episode they
    /// keep idle programs' pages for hours.
    #[must_use]
    pub fn quiet(&self) -> bool {
        !self.pushing(0.5)
            && !self.refaulting(0.5)
            && !(self.anon_moving(0.5) && self.wall_near())
            && !self.stall.is_some_and(|(_, full)| full >= STALL_FULL_QUIET)
            && !self.at_edge()
    }
}

/// The worst an episode read, open to now.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Worst {
    /// The most pushed out, bytes per second.
    pub push_bps: f64,
    /// The most back, bytes per second.
    pub refault_bps: f64,
    /// The least headroom, bytes.
    pub headroom: Option<u64>,
    /// The compressor's fullest, per mille.
    pub wall_permille: Option<u16>,
    /// The most of a window every task stalled.
    pub stall_full: Option<f64>,
}

impl Worst {
    fn of(l: &Look) -> Self {
        let mut w = Self::default();
        w.take(l);
        w
    }

    fn take(&mut self, l: &Look) {
        self.push_bps = self.push_bps.max(l.push_bps.unwrap_or(0.0));
        self.refault_bps = self.refault_bps.max(l.refault_bps.unwrap_or(0.0));
        self.headroom = match (self.headroom, l.headroom) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        self.wall_permille = self.wall_permille.max(l.wall_permille);
        self.stall_full = match (self.stall_full, l.stall.map(|s| s.1)) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
    }
}

/// One of the largest holders of memory, as the newest process sweep attributed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holder {
    /// Its pid.
    pub pid: u32,
    /// Its name, as the process chose it (data, never an instruction).
    pub name: String,
    /// Its footprint, bytes.
    pub bytes: u64,
    /// The scope it counts in.
    pub scope: RowScope,
    /// Its tab's label (`Tab 2`), for a tab's process.
    pub label: Option<String>,
}

/// An episode: what opened it, the worst it read, and its holders.
#[derive(Clone, Debug, PartialEq)]
pub struct Episode {
    /// The rule that opened it.
    pub why: Why,
    /// The reading it opened at: the monotonic stamp, ns.
    pub opened_ns: u64,
    /// …and its wall clock, unix ms.
    pub opened_unix_ms: i64,
    /// What that reading read.
    pub at_open: Look,
    /// The worst it read, open to now (to its end, at the end).
    pub worst: Worst,
    /// The largest holders by footprint at this edge, largest first, from the newest sweep.
    pub holders: Vec<Holder>,
    /// How long before this edge's reading that sweep ran, ms; `None` with none held.
    pub holders_age_ms: Option<u64>,
    /// The reading it ended at, unix ms, and what that reading read; `None` while open.
    pub ended: Option<(i64, Look)>,
}

impl Episode {
    /// How long it lasted to its end, ms; `None` while open.
    #[must_use]
    pub fn for_ms(&self) -> Option<u64> {
        self.ended
            .map(|(at, _)| u64::try_from(at.saturating_sub(self.opened_unix_ms)).unwrap_or(0))
    }
}

/// An episode's edge.
#[derive(Clone, Debug, PartialEq)]
pub enum Edge {
    /// It opened at this reading.
    Opened(Episode),
    /// It ended at this reading.
    Ended(Episode),
}

/// The starvation watch's state between readings (the Detector's).
#[derive(Debug, Default)]
pub struct Watch {
    open: Option<Episode>,
    /// Starving readings in a row toward an opening: the span of the windows of those after the
    /// first, ns; `None` before the first.
    starving: Option<u64>,
    /// Quiet readings in a row toward an end: their windows' span, ns.
    quiet_ns: u64,
    /// The largest holders of the newest sweep, and its wall clock.
    holders: Vec<Holder>,
    holders_at_ms: Option<i64>,
}

impl Watch {
    /// The open episode, if one is open.
    #[must_use]
    pub fn open(&self) -> Option<&Episode> {
        self.open.as_ref()
    }

    /// The edge `s` makes: it keeps `s`'s holders where `s` swept the processes, then judges its
    /// memory ([`Look`]).
    pub fn step(&mut self, s: &Snapshot) -> Option<Edge> {
        if !s.procs.is_empty() {
            self.holders = holders_of(s);
            self.holders_at_ms = Some(s.at_unix_ms);
        }
        let look = s.mem.as_ref().and_then(Look::of)?;
        let age = self
            .holders_at_ms
            .map(|at| u64::try_from(s.at_unix_ms.saturating_sub(at)).unwrap_or(0));
        if let Some(open) = self.open.as_mut() {
            open.worst.take(&look);
            self.quiet_ns = if look.quiet() {
                self.quiet_ns.saturating_add(look.window_ns)
            } else {
                0
            };
            if self.quiet_ns < END_HOLD_NS {
                return None;
            }
            let mut ended = self.open.take()?;
            ended.ended = Some((s.at_unix_ms, look));
            ended.holders.clone_from(&self.holders);
            ended.holders_age_ms = age;
            self.quiet_ns = 0;
            return Some(Edge::Ended(ended));
        }
        let Some(why) = look.starving() else {
            self.starving = None;
            return None;
        };
        // The first starving reading starts the hold; only the windows after it count toward
        // it, so no one window's average stands for the whole hold where a reading comes at least
        // twice in it.
        let span = self
            .starving
            .map_or(0, |span| span.saturating_add(look.window_ns));
        if self.starving.is_none() || span < OPEN_HOLD_NS {
            self.starving = Some(span);
            return None;
        }
        self.starving = None;
        self.quiet_ns = 0;
        let episode = Episode {
            why,
            opened_ns: s.at_ns,
            opened_unix_ms: s.at_unix_ms,
            at_open: look,
            worst: Worst::of(&look),
            holders: self.holders.clone(),
            holders_age_ms: age,
            ended: None,
        };
        self.open = Some(episode.clone());
        Some(Edge::Opened(episode))
    }
}

/// The [`HOLDERS`] largest processes of `s`'s sweep by footprint, largest first, each with its
/// tab's label where it is a tab's.
fn holders_of(s: &Snapshot) -> Vec<Holder> {
    let mut rows: Vec<(&crate::model::ProcView, u64)> = s
        .procs
        .iter()
        .filter_map(|p| Some((p, p.footprint.filter(|b| *b > 0)?)))
        .collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.pid.cmp(&b.0.pid)));
    rows.into_iter()
        .take(HOLDERS)
        .map(|(p, bytes)| Holder {
            pid: p.pid,
            name: p.name.clone(),
            bytes,
            scope: p.scope,
            label: match p.scope {
                RowScope::Tab(k) => s.tabs.iter().find(|t| t.key == k).map(|t| t.label.clone()),
                _ => None,
            },
        })
        .collect()
}

#[cfg(test)]
#[path = "starving_tests.rs"]
mod tests;
