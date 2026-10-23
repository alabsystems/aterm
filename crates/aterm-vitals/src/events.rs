// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! System state transition event detector ([`Detector`], [`JobWatch`]).
//! Thresholds are configured by [`SWAP_ACTIVE_BPS`] and [`LOAD_HOLD_NS`].

use std::collections::{HashMap, HashSet};

use crate::jobs::{Class, JobsView, Verdict};
use crate::model::{Pressure, RowScope, Snapshot, Throttle};

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
    /// The thermal level moved.
    Throttle {
        /// The level before.
        from: Throttle,
        /// The level now.
        to: Throttle,
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
            Self::Throttle { .. } => "throttle",
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
    throttle: Option<Throttle>,
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
        if let Some(t) = s.thermal {
            if let Some(before) = self.throttle.filter(|b| *b != t.level) {
                push(EventKind::Throttle {
                    from: before,
                    to: t.level,
                });
            }
            self.throttle = Some(t.level);
        }
        if let Some(v) = s.volume.as_ref().filter(|v| v.total > 0) {
            let free = v.available;
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
mod tests {
    use super::*;
    use crate::model::{
        CpuView, MemRates, MemSample, MemView, ScopeView, TabView, Thermal, VolumeSample,
    };
    use crate::trend::TrendView;

    fn snap(seq: u64, at_s: u64) -> Snapshot {
        Snapshot {
            seq,
            at_ns: at_s * 1_000_000_000,
            ..Snapshot::default()
        }
    }

    fn kinds(e: &[Event]) -> Vec<&'static str> {
        e.iter().map(|e| e.kind.word()).collect()
    }

    #[test]
    fn pressure_and_throttle_changes_are_events_and_the_first_reading_is_not() {
        let mut d = Detector::default();
        let mut s = snap(1, 1);
        s.mem = Some(MemView {
            sample: MemSample {
                total: 1,
                pressure: Pressure::Normal,
                ..MemSample::default()
            },
            ..MemView::default()
        });
        s.thermal = Some(Thermal::default());
        assert!(d.detect(&s).is_empty());
        let mut s2 = snap(2, 2);
        s2.mem = s.mem.clone();
        s2.mem.as_mut().unwrap().sample.pressure = Pressure::Warn;
        s2.thermal = Some(Thermal {
            level: Throttle::Heavy,
            raw: 2,
            low_power: None,
            exact: true,
        });
        let e = d.detect(&s2);
        assert_eq!(kinds(&e), ["pressure", "throttle"]);
        assert_eq!(e[0].seq, 2);
        assert_eq!(
            e[0].kind,
            EventKind::Pressure {
                from: Pressure::Normal,
                to: Pressure::Warn
            }
        );
        let mut s3 = snap(3, 3);
        s3.mem = s2.mem.clone();
        s3.mem.as_mut().unwrap().sample.pressure = Pressure::Unknown;
        assert!(d.detect(&s3).is_empty());
    }

    #[test]
    fn swap_traffic_starting_and_stopping_are_events() {
        let mut d = Detector::default();
        let at = |seq, bps: f64| {
            let mut s = snap(seq, seq);
            s.mem = Some(MemView {
                sample: MemSample {
                    total: 1,
                    ..MemSample::default()
                },
                rates: Some(MemRates {
                    swapin_bps: bps,
                    ..MemRates::default()
                }),
                ..MemView::default()
            });
            s
        };
        assert!(d.detect(&at(1, 0.0)).is_empty());
        let e = d.detect(&at(2, 5e6));
        assert_eq!(
            e[0].kind,
            EventKind::Swap {
                started: true,
                bps: 5_000_000
            }
        );
        assert!(d.detect(&at(3, 4e6)).is_empty());
        let e = d.detect(&at(4, 0.0));
        assert_eq!(
            e[0].kind,
            EventKind::Swap {
                started: false,
                bps: 0
            }
        );
    }

    #[test]
    fn free_space_crossing_a_line_either_way_is_an_event() {
        let mut d = Detector::default();
        let at = |seq, available: u64| {
            let mut s = snap(seq, seq);
            s.volume = Some(VolumeSample {
                total: 100_000_000_000,
                free: available,
                available,
                mount: "/".into(),
            });
            s
        };
        assert!(d.detect(&at(1, 50_000_000_000)).is_empty());
        let e = d.detect(&at(2, 9_000_000_000));
        assert_eq!(e.len(), 1);
        assert!(matches!(
            e[0].kind,
            EventKind::Disk {
                line: DiskLine::Pct10,
                below: true,
                ..
            }
        ));
        let e = d.detect(&at(3, 500_000_000));
        let lines: Vec<&str> = e
            .iter()
            .map(|e| match &e.kind {
                EventKind::Disk { line, .. } => line.word(),
                _ => "",
            })
            .collect();
        assert_eq!(lines, ["5%", "1GB"]);
        let e = d.detect(&at(4, 50_000_000_000));
        assert_eq!(e.len(), 3);
        assert!(
            e.iter()
                .all(|e| matches!(e.kind, EventKind::Disk { below: false, .. }))
        );
    }

    #[test]
    fn a_load_move_of_a_core_that_holds_ten_seconds_is_one_event() {
        let mut d = Detector::default();
        let at = |seq: u64, level: f32| {
            let mut s = snap(seq, seq);
            s.cpu = Some(CpuView {
                busy: TrendView::flat(level),
                ..CpuView::default()
            });
            s.global.top = Some(("trustc".into(), 2.0));
            s
        };
        assert!(d.detect(&at(1, 0.5)).is_empty());
        assert!(d.detect(&at(2, 3.0)).is_empty());
        assert!(d.detect(&at(3, 0.6)).is_empty());
        for t in 4..14 {
            assert!(d.detect(&at(t, 3.0)).is_empty(), "t={t}");
        }
        let e = d.detect(&at(14, 3.1));
        assert_eq!(
            e[0].kind,
            EventKind::Load {
                scope: LoadScope::Global,
                from_mc: 500,
                to_mc: 3100,
                top: Some("trustc".into()),
            }
        );
        for t in 15..40 {
            assert!(d.detect(&at(t, 3.1)).is_empty());
        }
    }

    #[test]
    fn tabs_opening_and_closing_are_events_after_the_first_sweep() {
        let mut d = Detector::default();
        let at = |seq, keys: &[u64]| {
            let mut s = snap(seq, seq);
            s.aterm = Some(ScopeView::default());
            s.tabs = keys
                .iter()
                .map(|&k| TabView {
                    key: k,
                    label: format!("Tab {k}"),
                    program: String::new(),
                    shell_pid: 0,
                    scope: ScopeView::default(),
                })
                .collect();
            s
        };
        assert!(d.detect(&at(1, &[1, 2])).is_empty());
        let e = d.detect(&at(2, &[2, 3]));
        assert_eq!(
            e.iter().map(|e| e.kind.clone()).collect::<Vec<_>>(),
            [
                EventKind::Tab {
                    key: 3,
                    label: "Tab 3".into(),
                    opened: true
                },
                EventKind::Tab {
                    key: 1,
                    label: "Tab 1".into(),
                    opened: false
                }
            ]
        );
        assert!(d.detect(&snap(3, 3)).iter().all(|e| e.kind.word() != "tab"));
    }

    #[test]
    fn a_reader_going_away_and_coming_back_is_a_lane_event() {
        let mut d = Detector::default();
        let mut s = snap(1, 1);
        s.mem = Some(MemView::default());
        assert!(d.detect(&s).is_empty());
        let e = d.detect(&snap(2, 2));
        assert!(e.iter().any(|e| e.kind
            == EventKind::Lane {
                lane: "mem",
                available: false
            }));
        let e = d.detect(&s);
        assert!(e.iter().any(|e| e.kind
            == EventKind::Lane {
                lane: "mem",
                available: true
            }));
    }

    #[test]
    fn a_closed_tab_whose_processes_live_on_is_an_orphan_event() {
        use crate::model::{ProcState, ProcView};
        let mut d = Detector::default();
        let tab = |k: u64| TabView {
            key: k,
            label: format!("Tab {k}"),
            program: String::new(),
            shell_pid: 0,
            scope: ScopeView::default(),
        };
        let proc = |pid: u32, name: &str, scope: RowScope, cores: f32| ProcView {
            pid,
            ppid: 1,
            pgid: pid,
            name: name.into(),
            cores: Some(cores),
            wait: None,
            footprint: None,
            peak: None,
            write_bps: None,
            state: ProcState::Run,
            scope,
            refused: false,
        };
        let mut s1 = snap(1, 1);
        s1.aterm = Some(ScopeView::default());
        s1.tabs = vec![tab(1), tab(2)];
        s1.procs = vec![
            proc(10, "sh", RowScope::Tab(2), 0.0),
            proc(11, "node", RowScope::Tab(2), 0.4),
            proc(12, "make", RowScope::Tab(2), 0.9),
            proc(20, "sh", RowScope::Tab(1), 0.0),
        ];
        assert!(d.detect(&s1).is_empty());
        let mut s2 = snap(2, 2);
        s2.aterm = Some(ScopeView::default());
        s2.tabs = vec![tab(1)];
        s2.procs = vec![
            proc(11, "node", RowScope::Others, 0.4),
            proc(12, "cc", RowScope::Others, 0.9),
            proc(20, "sh", RowScope::Tab(1), 0.0),
        ];
        let e = d.detect(&s2);
        assert_eq!(kinds(&e), ["tab", "orphan"]);
        assert_eq!(
            e[1].kind,
            EventKind::Orphan {
                key: 2,
                label: "Tab 2".into(),
                procs: 1,
                top: Some("node".into()),
            }
        );
        let mut s3 = snap(3, 3);
        s3.aterm = Some(ScopeView::default());
        s3.tabs = Vec::new();
        s3.procs = vec![proc(11, "node", RowScope::Others, 0.4)];
        assert_eq!(kinds(&d.detect(&s3)), ["tab"]);
    }

    #[test]
    fn a_job_whose_verdict_moves_is_a_job_event_and_a_steady_one_is_not() {
        use crate::jobs::JobsView;
        use crate::sys::jobs_view::fixtures::{row, stuck};
        let mut w = JobWatch::default();
        let view = |rows: Vec<crate::jobs::JobRow>| JobsView {
            rows,
            ..JobsView::default()
        };
        let e = w.step(
            &view(vec![
                row("a1", 7, Class::Running, Verdict::Fine),
                stuck("a2", 7),
            ]),
            5,
            9,
        );
        assert!(w.seeded());
        assert_eq!(e.len(), 1, "{e:?}");
        assert_eq!(e[0].seq, 5);
        assert!(matches!(
            &e[0].kind,
            EventKind::Job { id, from: None, to: Verdict::Stuck, class: Class::Waiting, .. } if id == "a2"
        ));
        let steady = view(vec![
            row("a1", 7, Class::Running, Verdict::Fine),
            stuck("a2", 7),
        ]);
        assert!(w.step(&steady, 6, 10).is_empty());
        let mut done = row("a1", 7, Class::Exited, Verdict::Exited);
        done.exit = Some(0);
        let e = w.step(
            &view(vec![
                done,
                stuck("a2", 7),
                row("a1", 8, Class::Running, Verdict::Fine),
            ]),
            7,
            11,
        );
        assert_eq!(e.len(), 1, "{e:?}");
        assert_eq!(
            e[0].kind,
            EventKind::Job {
                tab: 7,
                id: "a1".into(),
                from: Some(Verdict::Fine),
                to: Verdict::Exited,
                class: Class::Exited,
                task: Some("bq7f2k1za".into()),
                exit: Some(0),
            }
        );
        assert_eq!(e[0].kind.word(), "job");
    }

    #[test]
    fn millicores_round_and_never_go_negative() {
        assert_eq!(millicores(1.2345), 1235);
        assert_eq!(millicores(-1.0), 0);
        assert_eq!(millicores(f32::NAN), 0);
    }
}
