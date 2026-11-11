// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! System state transition event detector ([`Detector`], [`JobWatch`]).
//! Threshold constants: [`SWAP_ACTIVE_BPS`], [`LOAD_HOLD_NS`], [`LOAD_STEP_CORES`] and [`DISK_LINE_BYTES`].

use std::collections::{HashMap, HashSet};

use crate::jobs::{Class, JobsView, Verdict};
use crate::model::{Pressure, RowScope, Snapshot, Thermal};

/// Swap traffic (in plus out) at or above this is "swapping".
pub const SWAP_ACTIVE_BPS: f64 = 1_000_000.0;
/// A scope's move must hold this long to be a `load` event: 10 s.
pub const LOAD_HOLD_NS: u64 = 10_000_000_000;
/// …and be at least this many cores.
pub const LOAD_STEP_CORES: f32 = 1.0;
/// The free-space line in bytes: 1 GB.
pub const DISK_LINE_BYTES: u64 = 1_000_000_000;

/// One event.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    /// The sequence of the snapshot that raised it.
    pub seq: u64,
    /// That snapshot's monotonic stamp, ns.
    pub at_ns: u64,
    /// What happened.
    pub kind: EventKind,
}

/// Which scope a `load` event is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LoadScope {
    /// The machine.
    Global,
    /// This aterm instance.
    Aterm,
    /// A tab, by the host's key.
    Tab(u64),
}

/// Which free-space line a `disk` event crossed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskLine {
    /// 10 % of the volume.
    Pct10,
    /// 5 % of the volume.
    Pct5,
    /// 1 GB.
    Gb1,
}

impl DiskLine {
    /// The word the event prints (`line=10%`).
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Pct10 => "10%",
            Self::Pct5 => "5%",
            Self::Gb1 => "1GB",
        }
    }
}

/// What an event says.
#[derive(Clone, Debug, PartialEq)]
pub enum EventKind {
    /// Memory pressure moved.
    Pressure {
        /// The level before.
        from: Pressure,
        /// The level now.
        to: Pressure,
    },
    /// The thermal level moved; each side prints [`Thermal::word`].
    Thermal {
        /// The state before.
        from: Thermal,
        /// The state now.
        to: Thermal,
    },
    /// Swap traffic started or stopped.
    Swap {
        /// Started (true) or stopped.
        started: bool,
        /// The traffic now, bytes per second.
        bps: u64,
    },
    /// Free space crossed a line.
    Disk {
        /// The volume's mount point.
        mount: String,
        /// The line.
        line: DiskLine,
        /// Fell below it (true) or rose back above it.
        below: bool,
        /// Free bytes now.
        free: u64,
    },
    /// A scope's load moved and held.
    Load {
        /// Which scope.
        scope: LoadScope,
        /// Millicores before.
        from_mc: u32,
        /// Millicores now.
        to_mc: u32,
        /// The busiest process in it now, as the process named itself.
        top: Option<String>,
    },
    /// A tab opened or closed.
    Tab {
        /// The host's key.
        key: u64,
        /// Its label (`Tab 2`).
        label: String,
        /// Opened (true) or closed.
        opened: bool,
    },
    /// A reader became unavailable or available again.
    Lane {
        /// `cores`, `gpu`, `mem`, `procs` or `coalitions`.
        lane: &'static str,
        /// Available now.
        available: bool,
    },
    /// A tab closed and left processes behind.
    Orphan {
        /// The closed tab's key.
        key: u64,
        /// Its label (`Tab 2`).
        label: String,
        /// How many of its processes are still alive.
        procs: u32,
        /// The busiest of them, as the process named itself.
        top: Option<String>,
    },
    /// A job's verdict moved.
    Job {
        /// The tab's key.
        tab: u64,
        /// The job's id (`a<pgid>`, `b<block>`, `j<pid>`, `o<pid>`).
        id: String,
        /// The verdict before.
        from: Option<Verdict>,
        /// The verdict now.
        to: Verdict,
        /// Its class now.
        class: Class,
        /// The agent's task id, for an agent's call.
        task: Option<String>,
        /// The exit status, once known.
        exit: Option<i32>,
    },
}

impl EventKind {
    /// The kind word (`kind=`).
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            Self::Pressure { .. } => "pressure",
            Self::Thermal { .. } => "thermal",
            Self::Swap { .. } => "swap",
            Self::Disk { .. } => "disk",
            Self::Load { .. } => "load",
            Self::Tab { .. } => "tab",
            Self::Lane { .. } => "lane",
            Self::Orphan { .. } => "orphan",
            Self::Job { .. } => "job",
        }
    }
}

/// A scope's load watch: the level last reported, and a move in progress.
#[derive(Clone, Copy, Debug, Default)]
struct LoadWatch {
    reference: f32,
    /// The direction (+1 up, −1 down) and the stamp a move started.
    pending: Option<(i8, u64)>,
}

/// The detector's state between snapshots.
#[derive(Debug, Default)]
pub struct Detector {
    pressure: Option<Pressure>,
    thermal: Option<Thermal>,
    swapping: Option<bool>,
    disk: Option<(String, [bool; 3])>,
    load: HashMap<LoadScope, LoadWatch>,
    tabs: Option<HashMap<u64, String>>,
    lanes: [Option<bool>; 4],
    /// Whether the last facts' coalition lane read.
    coalitions: Option<bool>,
    /// Each tab's live processes in the last snapshot that swept them, for `orphan`.
    tab_procs: HashMap<u64, Vec<(u32, String, f32)>>,
}

impl Detector {
    /// The events `s` raises against what came before.
    pub fn detect(&mut self, s: &Snapshot) -> Vec<Event> {
        let mut out = Vec::new();
        let mut push = |kind| {
            out.push(Event {
                seq: s.seq,
                at_ns: s.at_ns,
                kind,
            });
        };
        if let Some(m) = s.mem.as_ref() {
            let now = m.sample.pressure;
            if now != Pressure::Unknown {
                if let Some(before) = self.pressure.filter(|b| *b != now) {
                    push(EventKind::Pressure {
                        from: before,
                        to: now,
                    });
                }
                self.pressure = Some(now);
            }
            if let Some(r) = m.rates {
                let bps = r.swap_bps();
                if bps.is_finite() {
                    let active = bps >= SWAP_ACTIVE_BPS;
                    if self.swapping.is_some_and(|was| was != active) {
                        push(EventKind::Swap {
                            started: active,
                            bps: bps as u64,
                        });
                    }
                    self.swapping = Some(active);
                }
            }
        }
        if let Some(t) = s.thermal {
            if let Some(before) = self.thermal.filter(|b| b.level != t.level) {
                push(EventKind::Thermal {
                    from: before,
                    to: t,
                });
            }
            self.thermal = Some(t);
        }
        if let Some(v) = s.volume.as_ref().filter(|v| v.total > 0) {
            let free = v.free;
            let below = [
                free.saturating_mul(10) < v.total,
                free.saturating_mul(20) < v.total,
                free < DISK_LINE_BYTES,
            ];
            if let Some((mount, was)) = self.disk.as_ref().filter(|(m, _)| *m == v.mount) {
                for (i, line) in [DiskLine::Pct10, DiskLine::Pct5, DiskLine::Gb1]
                    .into_iter()
                    .enumerate()
                {
                    if was[i] != below[i] {
                        push(EventKind::Disk {
                            mount: mount.clone(),
                            line,
                            below: below[i],
                            free,
                        });
                    }
                }
            }
            self.disk = Some((v.mount.clone(), below));
        }
        let mut scopes: Vec<(LoadScope, f32, Option<String>)> = Vec::new();
        if let Some(c) = s.cpu.as_ref() {
            scopes.push((
                LoadScope::Global,
                c.busy.level,
                s.global.top.as_ref().map(|t| t.0.clone()),
            ));
        }
        if let Some(a) = s.aterm.as_ref() {
            scopes.push((
                LoadScope::Aterm,
                a.cores.level,
                a.top.as_ref().map(|t| t.0.clone()),
            ));
            for t in &s.tabs {
                scopes.push((
                    LoadScope::Tab(t.key),
                    t.scope.cores.level,
                    t.scope.top.as_ref().map(|t| t.0.clone()),
                ));
            }
            let now: HashMap<u64, String> =
                s.tabs.iter().map(|t| (t.key, t.label.clone())).collect();
            if let Some(before) = self.tabs.as_ref() {
                let mut opened: Vec<(&u64, &String)> = now
                    .iter()
                    .filter(|(k, _)| !before.contains_key(k))
                    .collect();
                opened.sort();
                for (k, label) in opened {
                    push(EventKind::Tab {
                        key: *k,
                        label: label.clone(),
                        opened: true,
                    });
                }
                let mut closed: Vec<(&u64, &String)> = before
                    .iter()
                    .filter(|(k, _)| !now.contains_key(k))
                    .collect();
                closed.sort();
                for (k, label) in closed {
                    push(EventKind::Tab {
                        key: *k,
                        label: label.clone(),
                        opened: false,
                    });
                    if let Some((procs, top)) = self.left_behind(*k, s) {
                        push(EventKind::Orphan {
                            key: *k,
                            label: label.clone(),
                            procs,
                            top,
                        });
                    }
                }
            }
            self.tabs = Some(now);
        }
        if !s.procs.is_empty() {
            let mut by_tab: HashMap<u64, Vec<(u32, String, f32)>> = HashMap::new();
            for r in &s.procs {
                if let RowScope::Tab(k) = r.scope {
                    by_tab.entry(k).or_default().push((
                        r.pid,
                        r.name.clone(),
                        r.cores.unwrap_or(0.0),
                    ));
                }
            }
            self.tab_procs = by_tab;
        }
        let live: HashSet<LoadScope> = scopes.iter().map(|(k, _, _)| *k).collect();
        if s.aterm.is_some() {
            self.load.retain(|k, _| live.contains(k));
        }
        for (scope, level, top) in scopes {
            if !level.is_finite() {
                continue;
            }
            let Some(w) = self.load.get_mut(&scope) else {
                self.load.insert(
                    scope,
                    LoadWatch {
                        reference: level,
                        pending: None,
                    },
                );
                continue;
            };
            let delta = level - w.reference;
            if delta.abs() < LOAD_STEP_CORES {
                w.pending = None;
                continue;
            }
            let dir: i8 = if delta > 0.0 { 1 } else { -1 };
            match w.pending {
                Some((d, since)) if d == dir => {
                    if s.at_ns.saturating_sub(since) >= LOAD_HOLD_NS {
                        push(EventKind::Load {
                            scope,
                            from_mc: millicores(w.reference),
                            to_mc: millicores(level),
                            top,
                        });
                        w.reference = level;
                        w.pending = None;
                    }
                }
                _ => w.pending = Some((dir, s.at_ns)),
            }
        }
        let lanes = [
            ("cores", s.cpu.is_some(), s.cpu.is_some()),
            ("gpu", s.gpu.is_some(), true),
            ("mem", s.mem.is_some(), true),
            ("procs", s.global.procs > 0, true),
        ];
        for (i, (lane, now, seeds)) in lanes.into_iter().enumerate() {
            match self.lanes[i] {
                Some(was) if was != now => {
                    push(EventKind::Lane {
                        lane,
                        available: now,
                    });
                    self.lanes[i] = Some(now);
                }
                None if seeds => self.lanes[i] = Some(now),
                _ => {}
            }
        }
        if let Some(now) = s.why.as_ref().map(|w| w.lane.on()) {
            if self.coalitions.is_some_and(|was| was != now) {
                push(EventKind::Lane {
                    lane: "coalitions",
                    available: now,
                });
            }
            self.coalitions = Some(now);
        }
        out
    }
}

impl Detector {
    /// Counts processes from closed tab `key` still alive in `s` and returns the busiest name.
    fn left_behind(&self, key: u64, s: &Snapshot) -> Option<(u32, Option<String>)> {
        if s.procs.is_empty() {
            return None;
        }
        let held = self.tab_procs.get(&key)?;
        let mut alive: Vec<&(u32, String, f32)> = held
            .iter()
            .filter(|(pid, name, _)| s.procs.iter().any(|r| r.pid == *pid && r.name == *name))
            .collect();
        if alive.is_empty() {
            return None;
        }
        alive.sort_by(|a, b| b.2.total_cmp(&a.2));
        let n = u32::try_from(alive.len()).unwrap_or(u32::MAX);
        Some((n, alive.first().map(|a| a.1.clone())))
    }
}

/// Tracks job verdict transitions across stuck-engine views.
#[derive(Debug, Default)]
pub struct JobWatch {
    /// `(tab, id)` → the verdict last seen.
    seen: HashMap<(u64, String), Verdict>,
    /// Whether at least one view has been observed.
    seeded: bool,
}

impl JobWatch {
    /// The events `view` raises against the last view, numbered by snapshot `seq` taken at `at_ns`.
    pub fn step(&mut self, view: &JobsView, seq: u64, at_ns: u64) -> Vec<Event> {
        let mut out = Vec::new();
        let mut now: HashMap<(u64, String), Verdict> = HashMap::with_capacity(view.rows.len());
        for r in &view.rows {
            let key = (r.tab, r.id.clone());
            let before = self.seen.get(&key).copied();
            let moved = match before {
                Some(b) => b != r.verdict,
                None => r.verdict != Verdict::Fine,
            };
            if moved {
                out.push(Event {
                    seq,
                    at_ns,
                    kind: EventKind::Job {
                        tab: r.tab,
                        id: r.id.clone(),
                        from: before,
                        to: r.verdict,
                        class: r.class,
                        task: r.task.clone(),
                        exit: r.exit,
                    },
                });
            }
            now.insert(key, r.verdict);
        }
        self.seen = now;
        self.seeded = true;
        out
    }

    /// Whether a view was stepped yet.
    #[must_use]
    pub fn seeded(&self) -> bool {
        self.seeded
    }
}

/// Cores-worth as whole millicores, never negative.
#[must_use]
pub fn millicores(cores: f32) -> u32 {
    if cores.is_finite() && cores > 0.0 {
        (f64::from(cores) * 1000.0).round().min(f64::from(u32::MAX)) as u32
    } else {
        0
    }
}

#[cfg(test)]
#[path = "events_tests.rs"]
mod tests;
