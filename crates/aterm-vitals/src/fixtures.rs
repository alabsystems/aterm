// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The merged top deck's scenes (`docs/DESIGN-top-deck-merged-2026-10-21.md`): busy, calm and
//! absent [`Snapshot`]s carrying the numbers its mockups draw
//! (`docs/design-studies/top-deck-*-2026-10-23.png`), for the deck's tests, its gallery and a
//! dev scene. Built directly, not through [`crate::Vitals::absorb`], so every figure is the
//! mockup's own.
//!
//! The mockups' programs are listed as [`Program`]s, one process per tile, each named as the
//! mockup names it ("Spotlight", not `mdworker_shared`). Outside aterm only the Mac's top
//! programs are listed; inside it every process is, so all tabs' and each tab's counts are their
//! rows: busy's all tabs counts 21, one more than the mockup's 20, whose tiles leave Tab 2's
//! shell out. A program's age and the program groups are not on main yet; they join as they
//! land. The mockups draw no past pressure level, so Memory's kept pressure holds the scene's
//! level now over its whole run and its last two minutes.

use std::sync::Arc;

use crate::history::{Filling, FreeHistory, Rates};
use crate::jobs::{
    Class, Consumer, JobKind, JobRow, JobsView, Standing, Tick, Verdict, Wait, WaitName,
};
use crate::model::{
    AccountIx, Busiest, CapBasis, ClusterView, CoreView, CpuView, DiskFacts, DiskLevel, GpuSample,
    GpuView, LinkKind, LinkRates, LinkSample, LinkView, MemRates, MemSample, MemView, NetView,
    NoFigure, PlanFact, PlanRun, Pressure, ProcLimits, ProcScope, ProcState, ProcView, Resource,
    ResourceView, RowFacts, RowScope, ScopeFig, ScopeView, Series, Snapshot, Split, State, TabView,
    TopProc, VolumeSample, Writer,
};
use crate::peaks::{PlanDay, PlanWeek};
use crate::ring::{BUCKET_MS, Cell, RING_BUCKETS};
use crate::trend::{HISTORY_LEN, TrendView};
use crate::writes::ProgramHour;

/// A scene of the merged deck's mockups.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeckScene {
    /// A build saturating the Mac: CPU 99%, memory 88% and swapping, the disk full in 34 min,
    /// a password prompt in Tab 1 and a stuck `node` in the front tab.
    Busy,
    /// The same Mac idle: nothing needs you.
    Calm,
    /// Every optional reading absent, as at launch: the deck draws absences.
    Absent,
}

impl DeckScene {
    /// Every scene.
    pub const ALL: [Self; 3] = [Self::Busy, Self::Calm, Self::Absent];

    /// Its name, `busy`, `calm` or `absent`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::Calm => "calm",
            Self::Absent => "absent",
        }
    }
}

/// When every scene was read, unix ms.
pub const AT: i64 = 1_792_000_800_000 + 42 * 60_000 + 17_000;
/// Bytes in a GiB.
pub const GIB: f64 = 1_073_741_824.0;
/// The volume's size: 2 TB.
const DISK: u64 = 2_000_000_000_000;
/// The reference link: 1 Gb/s, bytes per second.
const LINK_BPS: f64 = 125e6;
/// The per-user process limit (`kern.maxprocperuid`) and the machine's.
const LIMITS: ProcLimits = ProcLimits {
    system: Some(16_000),
    per_user: Some(10_666),
};
const SELF_PID: u32 = 1000;

/// One tab of a scene's window, as its tab strip titles it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SceneTab {
    /// The title (`rustc`).
    pub title: &'static str,
    /// Its panes' session keys, the focused one first.
    pub panes: &'static [u64],
}

impl SceneTab {
    /// The tab's key ([`crate::model::TabRoot::tab`]): its least pane key.
    #[must_use]
    pub fn key(&self) -> u64 {
        self.panes.iter().copied().min().unwrap_or(0)
    }
}

const BUSY_TABS: &[SceneTab] = &[
    SceneTab {
        title: "claude",
        panes: &[1],
    },
    SceneTab {
        title: "zsh",
        panes: &[2],
    },
    SceneTab {
        title: "rustc",
        panes: &[3, 5],
    },
    SceneTab {
        title: "claude",
        panes: &[4],
    },
];

const CALM_TABS: &[SceneTab] = &[
    SceneTab {
        title: "claude",
        panes: &[1],
    },
    SceneTab {
        title: "claude",
        panes: &[2],
    },
    SceneTab {
        title: "zsh",
        panes: &[3],
    },
    SceneTab {
        title: "zsh",
        panes: &[4],
    },
];

/// The window's tabs, in strip order (absent shares busy's window).
#[must_use]
pub fn tabs(scene: DeckScene) -> &'static [SceneTab] {
    match scene {
        DeckScene::Busy | DeckScene::Absent => BUSY_TABS,
        DeckScene::Calm => CALM_TABS,
    }
}

/// The front tab's index in [`tabs`]: busy's Tab 3, calm's Tab 2.
#[must_use]
pub const fn front(scene: DeckScene) -> usize {
    match scene {
        DeckScene::Busy | DeckScene::Absent => 2,
        DeckScene::Calm => 1,
    }
}

/// Whose a [`Program`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    /// Outside aterm, in this scope.
    Outside(RowScope),
    /// aterm itself.
    Aterm,
    /// A pane, by its session key.
    Pane(u64),
}

impl Owner {
    /// Its scope as the busiest and the writers name it in `scene`: a pane's is its tab's
    /// ([`tab_scope`]).
    fn scope(self, scene: DeckScene) -> ProcScope {
        match self {
            Self::Pane(k) => tab_scope(scene, k),
            Self::Aterm => ProcScope::Aterm,
            Self::Outside(_) => ProcScope::Other,
        }
    }
}

/// One program as the mockups draw it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Program {
    /// Its name.
    pub name: &'static str,
    /// Whose it is.
    pub owner: Owner,
    /// Each process's CPU, cores (a tile each in the mockup's chip).
    pub cores: &'static [f32],
    /// Its memory, GiB, split evenly over its processes.
    pub gib: f64,
}

const fn prog(name: &'static str, owner: Owner, cores: &'static [f32], gib: f64) -> Program {
    Program {
        name,
        owner,
        cores,
        gib,
    }
}

/// Busy, each row in the mockup's rank (by CPU, the Mac's nearer capacity: 99% over 88%). CPU
/// outside the tabs 99% - 57% of 18 = 7.6 cores, of which these read 6.7; the other tabs and
/// aterm 0.76 of 0.9; the front tab 7.9 of 9.4.
const BUSY_PROGRAMS: &[Program] = &[
    prog(
        "Spotlight",
        Owner::Outside(RowScope::Others),
        &[0.98, 0.96, 0.93, 0.88, 0.71, 0.45, 0.22],
        1.2,
    ),
    prog(
        "Docker",
        Owner::Outside(RowScope::Others),
        &[0.62, 0.35, 0.18],
        24.0,
    ),
    prog(
        "WindowServer",
        Owner::Outside(RowScope::Services),
        &[0.45],
        0.9,
    ),
    prog("claude", Owner::Pane(1), &[0.30], 18.9),
    prog("node", Owner::Pane(1), &[0.12], 3.09),
    prog("sudo", Owner::Pane(1), &[0.05], 0.01),
    prog("claude", Owner::Pane(4), &[0.15], 19.6),
    prog("node", Owner::Pane(4), &[0.04], 0.4),
    prog("aterm", Owner::Aterm, &[0.10], 1.1),
    prog("zsh", Owner::Pane(2), &[0.0], 0.01),
    prog(
        "rustc",
        Owner::Pane(3),
        &[0.98, 1.0, 0.96, 1.0, 0.93, 0.99, 1.0, 0.62, 0.41],
        15.6,
    ),
    prog("node", Owner::Pane(5), &[0.0, 0.01], 1.1),
    prog("cargo", Owner::Pane(3), &[0.0], 0.08),
    prog("zsh", Owner::Pane(3), &[0.0], 0.01),
    prog("zsh", Owner::Pane(5), &[0.0], 0.01),
];

/// Calm, ranked by memory (24% over the CPU's 6%): the Mac's top is Safari, 6.1 GiB.
const CALM_PROGRAMS: &[Program] = &[
    prog(
        "Safari",
        Owner::Outside(RowScope::Others),
        &[0.06, 0.02, 0.01, 0.0],
        6.1,
    ),
    prog(
        "WindowServer",
        Owner::Outside(RowScope::Services),
        &[0.08],
        0.7,
    ),
    prog("claude", Owner::Pane(1), &[0.04], 3.0),
    prog("node", Owner::Pane(1), &[0.01, 0.0, 0.0], 0.6),
    prog("aterm", Owner::Aterm, &[0.03], 1.1),
    prog("zsh", Owner::Pane(3), &[0.0], 0.02),
    prog("vim", Owner::Pane(3), &[0.0], 0.18),
    prog("zsh", Owner::Pane(4), &[0.0], 0.02),
    prog("tail", Owner::Pane(4), &[0.0, 0.0], 0.02),
    prog("ssh", Owner::Pane(4), &[0.0], 0.02),
    prog("caffeinate", Owner::Pane(4), &[0.0], 0.01),
    prog("claude", Owner::Pane(2), &[0.18], 1.4),
    prog("node", Owner::Pane(2), &[0.01, 0.0, 0.02], 0.3),
    prog("zsh", Owner::Pane(2), &[0.0], 0.02),
    prog("caffeinate", Owner::Pane(2), &[0.0], 0.01),
];

/// The scene's programs, each scope row's in the deck's rank.
#[must_use]
pub fn programs(scene: DeckScene) -> &'static [Program] {
    match scene {
        DeckScene::Busy => BUSY_PROGRAMS,
        DeckScene::Calm => CALM_PROGRAMS,
        DeckScene::Absent => &[],
    }
}

/// A resource's 10 minutes, 20 points of 30 s, as shares of its capacity.
type Run = [f32; 20];

/// One resource as the mockup draws it, in shares of its capacity.
struct Lane {
    /// The Mac's share now.
    now: f64,
    /// The slow average (the mockup's ghost).
    ghost: f32,
    /// The variance's band, low and high.
    spread: (f32, f32),
    /// The Mac's 10 minutes.
    run: Run,
    /// The points past capacity.
    over: &'static [usize],
    /// All tabs' and this tab's shares now and their runs.
    scopes: [(f64, Run); 2],
}

/// Everything a scene draws.
struct Numbers {
    scene: DeckScene,
    cpu: Lane,
    /// The Super then the Performance cores: each one's busy fraction, and its slow average.
    cores: [(&'static [f32], &'static [f32]); 2],
    load: [f64; 3],
    mem: Lane,
    /// Memory: in use (app and wired), compressed, swap, GiB of 128.
    mem_gib: (f64, f64, f64),
    pressure: Pressure,
    gpu: Lane,
    net: Lane,
    /// The Processes row: yours as a share of the per-user limit over 10 minutes.
    procs: Run,
    /// The machine's processes, and yours.
    counts: (u64, u64),
    /// Free space, GB, each hour's least, the last one now.
    free_gb: [f64; 24],
    /// Its top writer, GB an hour.
    writer: (&'static str, f64),
    /// Plan used each of the last 7 days, and the day the week reset.
    plan: ([u16; 7], usize),
    /// Output tokens a minute now, and the 7-day peak.
    tokens: (f64, f64),
}

const fn flat(v: f32) -> Run {
    [v; 20]
}

const BUSY: Numbers = Numbers {
    scene: DeckScene::Busy,
    cpu: Lane {
        now: 0.99,
        ghost: 0.86,
        spread: (0.93, 1.0),
        run: [
            0.36, 0.42, 0.40, 0.33, 0.41, 0.44, 0.38, 0.46, 0.55, 0.63, 0.76, 0.88, 0.94, 0.97,
            0.99, 1.0, 1.0, 0.99, 1.0, 0.99,
        ],
        over: &[13, 14, 15, 16, 17, 18, 19],
        scopes: [
            (
                0.57,
                [
                    0.08, 0.10, 0.09, 0.08, 0.10, 0.12, 0.10, 0.14, 0.20, 0.27, 0.36, 0.45, 0.50,
                    0.53, 0.55, 0.56, 0.57, 0.56, 0.57, 0.57,
                ],
            ),
            (
                0.52,
                [
                    0.02, 0.03, 0.02, 0.02, 0.03, 0.04, 0.03, 0.07, 0.14, 0.21, 0.30, 0.39, 0.44,
                    0.47, 0.49, 0.51, 0.52, 0.51, 0.52, 0.52,
                ],
            ),
        ],
    },
    cores: [
        (
            &[1.0, 0.99, 1.0, 0.98, 1.0, 0.99],
            &[0.86, 0.90, 0.84, 0.92, 0.88, 0.90],
        ),
        (
            &[
                0.97, 1.0, 0.94, 0.99, 1.0, 0.92, 0.96, 1.0, 0.98, 0.90, 1.0, 0.95,
            ],
            &[
                0.74, 0.80, 0.70, 0.85, 0.78, 0.66, 0.72, 0.81, 0.76, 0.69, 0.83, 0.71,
            ],
        ),
    ],
    load: [25.7, 21.4, 13.9],
    mem: Lane {
        now: 113.0 / 128.0,
        ghost: 0.84,
        spread: (0.86, 0.89),
        run: [
            0.70, 0.70, 0.71, 0.73, 0.74, 0.74, 0.76, 0.77, 0.78, 0.80, 0.81, 0.83, 0.84, 0.85,
            0.86, 0.86, 0.87, 0.88, 0.88, 0.88,
        ],
        over: &[15, 16, 17, 18, 19],
        scopes: [
            (
                0.48,
                [
                    0.40, 0.40, 0.41, 0.41, 0.42, 0.42, 0.43, 0.43, 0.44, 0.44, 0.45, 0.45, 0.46,
                    0.46, 0.47, 0.47, 0.47, 0.48, 0.48, 0.48,
                ],
            ),
            (
                0.14,
                [
                    0.06, 0.06, 0.07, 0.07, 0.08, 0.08, 0.09, 0.09, 0.10, 0.10, 0.11, 0.11, 0.12,
                    0.12, 0.13, 0.13, 0.13, 0.14, 0.14, 0.14,
                ],
            ),
        ],
    },
    mem_gib: (104.4, 8.6, 6.0),
    pressure: Pressure::Warn,
    gpu: Lane {
        now: 0.12,
        ghost: 0.11,
        spread: (0.07, 0.16),
        run: [
            0.09, 0.12, 0.06, 0.11, 0.14, 0.09, 0.07, 0.12, 0.15, 0.10, 0.08, 0.13, 0.11, 0.10,
            0.14, 0.12, 0.09, 0.13, 0.12, 0.12,
        ],
        over: &[],
        scopes: [
            (
                0.03,
                [
                    0.02, 0.03, 0.02, 0.03, 0.03, 0.02, 0.03, 0.04, 0.03, 0.03, 0.02, 0.03, 0.04,
                    0.03, 0.03, 0.03, 0.02, 0.03, 0.03, 0.03,
                ],
            ),
            (0.0, flat(0.0)),
        ],
    },
    net: Lane {
        now: 0.12,
        ghost: 0.08,
        spread: (0.03, 0.26),
        run: [
            0.05, 0.03, 0.04, 0.12, 0.22, 0.25, 0.18, 0.09, 0.07, 0.06, 0.10, 0.14, 0.20, 0.13,
            0.09, 0.16, 0.12, 0.10, 0.14, 0.12,
        ],
        over: &[],
        scopes: [(0.0, flat(0.0)), (0.0, flat(0.0))],
    },
    procs: [
        0.055, 0.055, 0.056, 0.055, 0.056, 0.057, 0.056, 0.058, 0.060, 0.061, 0.062, 0.061, 0.060,
        0.060, 0.061, 0.060, 0.060, 0.061, 0.060, 0.060,
    ],
    counts: (897, 640),
    free_gb: [
        210.0, 208.0, 205.0, 203.0, 200.0, 197.0, 193.0, 190.0, 186.0, 182.0, 178.0, 174.0, 170.0,
        166.0, 162.0, 158.0, 154.0, 150.0, 146.0, 140.0, 132.0, 122.0, 110.0, 40.0,
    ],
    writer: ("rustc", 68.0),
    plan: ([480, 590, 700, 790, 120, 240, 340], 4),
    tokens: (2_100.0, 9_000.0),
};

const CALM: Numbers = Numbers {
    scene: DeckScene::Calm,
    cpu: Lane {
        now: 0.06,
        ghost: 0.07,
        spread: (0.04, 0.09),
        run: [
            0.07, 0.06, 0.08, 0.05, 0.06, 0.07, 0.09, 0.06, 0.05, 0.06, 0.07, 0.05, 0.06, 0.08,
            0.07, 0.06, 0.05, 0.06, 0.07, 0.06,
        ],
        over: &[],
        scopes: [(0.02, flat(0.02)), (0.01, flat(0.01))],
    },
    cores: [
        (
            &[0.12, 0.05, 0.08, 0.03, 0.02, 0.06],
            &[0.10, 0.06, 0.07, 0.03, 0.02, 0.05],
        ),
        (
            &[
                0.04, 0.02, 0.15, 0.01, 0.03, 0.0, 0.02, 0.06, 0.01, 0.0, 0.03, 0.02,
            ],
            &[
                0.05, 0.02, 0.30, 0.01, 0.03, 0.0, 0.02, 0.05, 0.01, 0.0, 0.02, 0.02,
            ],
        ),
    ],
    load: [2.1, 2.4, 2.6],
    mem: Lane {
        now: 31.1 / 128.0,
        ghost: 0.24,
        spread: (0.235, 0.245),
        run: flat(0.24),
        over: &[],
        scopes: [(0.06, flat(0.056)), (0.02, flat(0.016))],
    },
    mem_gib: (31.0, 0.1, 0.0),
    pressure: Pressure::Normal,
    gpu: Lane {
        now: 0.03,
        ghost: 0.03,
        spread: (0.02, 0.05),
        run: [
            0.03, 0.02, 0.04, 0.03, 0.02, 0.03, 0.05, 0.03, 0.02, 0.03, 0.03, 0.02, 0.04, 0.03,
            0.02, 0.03, 0.03, 0.02, 0.03, 0.03,
        ],
        over: &[],
        scopes: [(0.01, flat(0.01)), (0.0, flat(0.0))],
    },
    net: Lane {
        now: 0.01,
        ghost: 0.01,
        spread: (0.0, 0.03),
        run: [
            0.02, 0.01, 0.01, 0.02, 0.01, 0.03, 0.02, 0.01, 0.02, 0.01, 0.02, 0.02, 0.01, 0.03,
            0.02, 0.02, 0.01, 0.01, 0.02, 0.01,
        ],
        over: &[],
        scopes: [(0.0, flat(0.0)), (0.0, flat(0.0))],
    },
    procs: [
        0.038, 0.038, 0.039, 0.038, 0.038, 0.039, 0.038, 0.038, 0.038, 0.039, 0.038, 0.038, 0.038,
        0.039, 0.038, 0.038, 0.038, 0.038, 0.038, 0.038,
    ],
    counts: (612, 427),
    free_gb: [
        884.0, 884.0, 884.0, 883.0, 883.0, 883.0, 883.0, 882.0, 882.0, 882.0, 882.0, 882.0, 881.0,
        881.0, 881.0, 881.0, 881.0, 881.0, 880.0, 880.0, 880.0, 880.0, 880.0, 880.0,
    ],
    writer: ("Safari", 0.2),
    plan: ([620, 710, 780, 80, 160, 250, 340], 3),
    tokens: (400.0, 9_000.0),
};

/// The scene as one snapshot at [`AT`].
#[must_use]
pub fn snapshot(scene: DeckScene) -> Snapshot {
    match scene {
        DeckScene::Busy => built(&BUSY),
        DeckScene::Calm => built(&CALM),
        DeckScene::Absent => absent(),
    }
}

/// The tab a pane sits in, and its index.
fn pane_tab(scene: DeckScene, key: u64) -> Option<(usize, &'static SceneTab)> {
    tabs(scene)
        .iter()
        .enumerate()
        .find(|(_, t)| t.panes.contains(&key))
}

/// The scope of a process in pane `key`, as the model names it: its tab's key
/// ([`ProcScope::Tab`]), so a program in both panes of a split tab is one tab's.
fn tab_scope(scene: DeckScene, key: u64) -> ProcScope {
    ProcScope::Tab(pane_tab(scene, key).map_or(key, |(_, t)| t.key()))
}

/// Every pane of the window, in strip order.
fn panes(scene: DeckScene) -> impl Iterator<Item = u64> {
    tabs(scene).iter().flat_map(|t| t.panes.iter().copied())
}

fn ring_k() -> i64 {
    AT.div_euclid(BUCKET_MS)
}

/// An hour of 10 s cells ending at [`AT`]: 50 minutes near the run's first point, then its 20
/// points of 30 s, three cells each, `over` marking the points past capacity.
fn ring(run: &Run, cap: f64, over: &[usize]) -> Arc<[Cell]> {
    let before = RING_BUCKETS - 3 * run.len();
    let cell = |v: f64, over: bool| Cell {
        mean: Some(v),
        max: Some(v),
        over,
    };
    (0..before)
        .map(|i| {
            cell(
                f64::from(run[0]) * cap * (1.0 + 0.04 * (i as f64 * 0.7).sin()),
                false,
            )
        })
        .chain(
            run.iter()
                .enumerate()
                .flat_map(|(j, &v)| [cell(f64::from(v) * cap, over.contains(&j)); 3]),
        )
        .collect()
}

/// A scope's figure: `now` and the last 10 minutes, each a share of `cap` scaled by `part`.
fn fig(now: f64, run: &Run, cap: f64, part: f64) -> ScopeFig {
    ScopeFig {
        now: Some(now * cap * part),
        missing: None,
        run: run
            .iter()
            .flat_map(|&v| {
                let v = f64::from(v) * cap * part;
                [Cell {
                    mean: Some(v),
                    max: Some(v),
                    over: false,
                }; 3]
            })
            .collect(),
        run_k: ring_k(),
    }
}

/// A quantity read rising (or falling) from its slow average, with its spread.
fn trend(level: f32, slow: f32, spread: (f32, f32)) -> TrendView {
    TrendView {
        raw: level,
        level,
        slow,
        mad: (spread.1 - spread.0) / 2.0,
    }
}

/// The level Memory's kept pressure holds in each second and cell: the scene's level now,
/// hollow where it reads none.
fn kept_pressure(n: &Numbers) -> Option<Pressure> {
    n.pressure.rank().map(|_| n.pressure)
}

/// The last two minutes, a second each, from the run's last four points.
fn history(run: &Run, scale: f32) -> Vec<Option<f32>> {
    let tail = &run[run.len() - 4..];
    (0..HISTORY_LEN)
        .map(|i| Some(tail[i * tail.len() / HISTORY_LEN] * scale))
        .collect()
}

/// A row: its figure now against `cap`, its hour, and a second series where it has one.
fn view(
    now: f64,
    cap: f64,
    ring: Arc<[Cell]>,
    over: bool,
    basis: CapBasis,
    second: Option<Series>,
) -> ResourceView {
    let a = (now / cap) as f32;
    let b = second
        .as_ref()
        .and_then(|s| Some((s.now? / s.capacity?) as f32));
    let binds = b.is_some_and(|b| b > a);
    ResourceView {
        now: Some(now),
        capacity: Some(cap),
        heat_now: Some(b.map_or(a, |b| a.max(b)).min(basis.heat_ceiling())),
        over,
        ring,
        ring_k: ring_k(),
        basis,
        second: second.map(|s| Series { binds, ..s }),
        ..ResourceView::default()
    }
}

/// Each pane's weight in a resource and aterm's own, from the programs: how a scope's figure
/// splits over the panes and aterm.
fn weights(n: &Numbers, of: impl Fn(&Program) -> f64) -> (Vec<(u64, f64)>, f64) {
    let progs = programs(n.scene);
    let sum = |owner: Owner| -> f64 { progs.iter().filter(|p| p.owner == owner).map(&of).sum() };
    (
        panes(n.scene).map(|k| (k, sum(Owner::Pane(k)))).collect(),
        sum(Owner::Aterm),
    )
}

/// A row's scope figures: all tabs' (`aterm`) and each pane's, the front tab's share split over
/// its panes and the rest over the other panes and aterm, both by the programs' weights.
fn scope_figs(
    n: &Numbers,
    lane: &Lane,
    cap: f64,
    of: impl Fn(&Program) -> f64,
) -> (ScopeFig, Vec<(u64, ScopeFig)>) {
    let front = tabs(n.scene)[front(n.scene)].panes;
    let (panes, own) = weights(n, of);
    let [(tabs_now, tabs_run), (tab_now, tab_run)] = &lane.scopes;
    let rest: Run = std::array::from_fn(|i| (tabs_run[i] - tab_run[i]).max(0.0));
    let tab_w: f64 = panes
        .iter()
        .filter(|(k, _)| front.contains(k))
        .map(|p| p.1)
        .sum();
    let rest_w: f64 = own
        + panes
            .iter()
            .filter(|(k, _)| !front.contains(k))
            .map(|p| p.1)
            .sum::<f64>();
    let share = |w: f64, of: f64| if of > 0.0 { w / of } else { 0.0 };
    let figs = panes
        .iter()
        .map(|&(k, w)| {
            let f = if front.contains(&k) {
                fig(*tab_now, tab_run, cap, share(w, tab_w))
            } else {
                fig(tabs_now - tab_now, &rest, cap, share(w, rest_w))
            };
            (k, f)
        })
        .collect();
    (fig(*tabs_now, tabs_run, cap, 1.0), figs)
}

fn cores_of(p: &Program) -> f64 {
    p.cores.iter().copied().map(f64::from).sum()
}

fn procs_of(p: &Program) -> f64 {
    p.cores.len() as f64
}

/// The scene's processes, one per tile: each program's first leads its process group, the rest
/// are its children, and a leader is aterm's child in a pane, launchd's outside aterm.
fn procs(n: &Numbers) -> Vec<ProcView> {
    let (writer, gb_h) = n.writer;
    let writer_cores: f64 = programs(n.scene)
        .iter()
        .filter(|p| p.name == writer)
        .map(cores_of)
        .sum();
    let mut next = SELF_PID;
    let mut out = Vec::new();
    for p in programs(n.scene) {
        let scope = match p.owner {
            Owner::Outside(scope) => scope,
            Owner::Aterm => RowScope::SelfProc,
            Owner::Pane(k) => RowScope::Tab(k),
        };
        let bytes = (p.gib * GIB / p.cores.len() as f64) as u64;
        let leader = if p.owner == Owner::Aterm {
            SELF_PID
        } else {
            next + 1
        };
        let parent = match p.owner {
            Owner::Pane(_) => SELF_PID,
            Owner::Aterm | Owner::Outside(_) => 1,
        };
        for &c in p.cores {
            let pid = if p.owner == Owner::Aterm {
                SELF_PID
            } else {
                next += 1;
                next
            };
            let writes = (p.name == writer && writer_cores > 0.0)
                .then(|| gb_h * 1e9 / 3600.0 * f64::from(c) / writer_cores);
            out.push(ProcView {
                pid,
                ppid: if pid == leader { parent } else { leader },
                pgid: leader,
                name: p.name.into(),
                cores: Some(c),
                wait: None,
                footprint: Some(bytes),
                peak: Some(bytes),
                write_bps: Some(writes.unwrap_or(0.0)),
                state: ProcState::Run,
                scope,
                refused: false,
                gpu: None,
                birth_ms: None,
            });
        }
    }
    out
}

/// A job row as the engine leaves it.
fn job(tab: u64, leader: u32, exe: &str, members: usize, verdict: Verdict) -> JobRow {
    JobRow {
        id: format!("{tab}:{leader}"),
        tab,
        kind: JobKind::Shell,
        consumer: Consumer::PersonFg,
        promoted_ms: None,
        task: None,
        pgid: leader,
        leader: (leader, None),
        exe: exe.into(),
        members,
        class: Class::Running,
        wait: None,
        named: None,
        stop: None,
        input: None,
        cpu_mc: None,
        zombies: None,
        standing: None,
        quiet_ms: None,
        phi_milli: None,
        mu_ms: None,
        mu_from: None,
        verdict,
        no_progress: false,
        episode: 0,
        exit: None,
        tick: Tick::default(),
    }
}

/// Busy's jobs: Tab 1's `sudo` waits for a password, the front tab's `cargo build` runs and its
/// `node` has been stuck on a TCP read for 12 minutes; calm's: nothing to say.
fn jobs(n: &Numbers, procs: &[ProcView]) -> JobsView {
    if n.scene != DeckScene::Busy {
        return JobsView::default();
    }
    let leader = |name: &str, tab: u64| {
        procs
            .iter()
            .find(|p| p.name == name && p.scope == RowScope::Tab(tab))
            .map_or(0, |p| p.pgid)
    };
    let mut password = job(1, leader("sudo", 1), "sudo", 1, Verdict::WaitingYou);
    password.class = Class::Waiting;
    password.wait = Some(Wait::bare(
        WaitName::TtyPassword,
        Standing::Legit,
        password.pgid,
        "sudo",
    ));
    password.standing = Some(Standing::Legit);
    password.quiet_ms = Some(40_000);
    password.phi_milli = Some(0);
    let mut build = job(3, leader("cargo", 3), "cargo", 11, Verdict::Fine);
    build.cpu_mc = Some(7_890);
    let mut stuck = job(5, leader("node", 5), "node", 2, Verdict::Stuck);
    stuck.class = Class::Waiting;
    stuck.wait = Some(Wait::bare(
        WaitName::Tcp,
        Standing::Capable,
        stuck.pgid,
        "node",
    ));
    stuck.standing = Some(Standing::Capable);
    stuck.quiet_ms = Some(12 * 60_000);
    stuck.phi_milli = Some(9_400);
    JobsView {
        rows: vec![password, build, stuck],
        ..JobsView::default()
    }
}

/// The plan's week: each day's newest reading, the reset day keeping the old week's last too.
fn plan_run(n: &Numbers) -> PlanRun {
    let today = u32::try_from(AT.div_euclid(86_400_000)).unwrap_or(0);
    let (used, reset) = n.plan;
    let day_s = |d: u32| i64::from(d) * 86_400;
    let first = today - 6;
    let reset_at = day_s(first + reset as u32) + 9 * 3_600;
    let mut days = Vec::new();
    for (i, &u) in used.iter().enumerate() {
        let day = first + i as u32;
        let at = if day == today {
            AT / 1000 - 240
        } else {
            day_s(day) + 23 * 3_600
        };
        if i == reset {
            days.push(PlanDay {
                day,
                used_x10: used[i - 1] + 30,
                resets_at: reset_at,
                at: reset_at - 3_600,
            });
        }
        days.push(PlanDay {
            day,
            used_x10: u,
            resets_at: if i < reset {
                reset_at
            } else {
                reset_at + 7 * 86_400
            },
            at,
        });
    }
    PlanRun {
        week: PlanWeek {
            account: "c-0123456789ab".into(),
            days,
        },
        front: n.scene == DeckScene::Calm,
        today,
    }
}

/// The volume's free space over 24 hours, a reading every 10 minutes, each hour falling to its
/// least by its end.
fn free_history(free_gb: &[f64; 24]) -> FreeHistory {
    let mut h = FreeHistory::new();
    let start = AT - 24 * 3_600_000;
    for b in 0..144_usize {
        let hour = b / 6;
        let from = if hour == 0 {
            free_gb[0]
        } else {
            free_gb[hour - 1]
        };
        let v = from + (free_gb[hour] - from) * ((b % 6 + 1) as f64 / 6.0);
        h.note_on(
            "/",
            DISK,
            start + (b as i64 + 1) * 600_000,
            (v * 1e9) as u64,
        );
    }
    h
}

fn built(n: &Numbers) -> Snapshot {
    let scene = n.scene;
    let busy = scene == DeckScene::Busy;
    let procs = procs(n);
    let jobs = jobs(n, &procs);
    let gib = |p: &Program| p.gib;

    let n_cores = n.cores.iter().map(|c| c.0.len()).sum::<usize>() as f64;
    let mut cores = Vec::new();
    let mut clusters = Vec::new();
    for ((levels, slows), (name, short)) in
        n.cores.iter().zip([("Super", "S"), ("Performance", "P")])
    {
        let first = cores.len();
        cores.extend(levels.iter().zip(*slows).map(|(&level, &slow)| CoreView {
            load: trend(level, slow, (level - 0.02, level)),
        }));
        clusters.push(ClusterView {
            name: name.into(),
            short: short.into(),
            cores: (first..cores.len()).collect(),
        });
    }
    let core_history = cores
        .iter()
        .map(|c: &CoreView| {
            (0..60)
                .map(|i| Some(c.load.slow + (c.load.level - c.load.slow) * i as f32 / 59.0))
                .collect()
        })
        .collect();
    let cpu = CpuView {
        busy: trend(
            (n.cpu.now * n_cores) as f32,
            n.cpu.ghost * n_cores as f32,
            (
                n.cpu.spread.0 * n_cores as f32,
                n.cpu.spread.1 * n_cores as f32,
            ),
        ),
        history: history(&n.cpu.run, n_cores as f32),
        cores,
        clusters,
        core_history,
    };

    let (used, compressed, swap) = n.mem_gib;
    let wired = if busy { 7.4 } else { 5.0 };
    let cached = if busy { 11.0 } else { 52.0 };
    let mem_total = 128.0 * GIB;
    let sample = MemSample {
        total: mem_total as u64,
        app: ((used - wired) * GIB) as u64,
        wired: (wired * GIB) as u64,
        compressed: Some((compressed * GIB) as u64),
        cached: (cached * GIB) as u64,
        free: ((128.0 - used - compressed - cached) * GIB) as u64,
        swap_used: (swap * GIB) as u64,
        swap_total: if swap > 0.0 {
            ((swap + 1.0) * GIB) as u64
        } else {
            0
        },
        pressure: n.pressure,
        ..MemSample::default()
    };
    let swap_bps = if busy { 36.0 * 1_048_576.0 } else { 0.0 };
    let mem = MemView {
        sample,
        used_gib: trend(
            (n.mem.now * 128.0) as f32,
            n.mem.ghost * 128.0,
            (n.mem.spread.0 * 128.0, n.mem.spread.1 * 128.0),
        ),
        history: history(&n.mem.run, 128.0),
        pressure_history: vec![kept_pressure(n); HISTORY_LEN],
        rates: Some(MemRates {
            swapin_bps: swap_bps / 3.0,
            swapout_bps: swap_bps * 2.0 / 3.0,
            window_ns: 1_000_000_000,
            ..MemRates::default()
        }),
    };

    let gpu_sample = GpuSample {
        device_pct: Some((n.gpu.now * 100.0).round() as u8),
        cores: Some(40),
        name: Some("Apple M5 Max".into()),
        in_use_bytes: Some((1.7 * GIB) as u64),
        alloc_bytes: Some((2.4 * GIB) as u64),
        recoveries: Some(0),
        renderer_pct: Some((n.gpu.now * 90.0).round() as u8),
        tiler_pct: Some((n.gpu.now * 40.0).round() as u8),
    };
    let gpu = GpuView {
        load: trend(n.gpu.now as f32, n.gpu.ghost, n.gpu.spread),
        cores: Some(40),
        name: gpu_sample.name.clone(),
        history: history(&n.gpu.run, 1.0),
        sample: gpu_sample.clone(),
        split: None,
    };

    let free = (n.free_gb[23] * 1e9) as u64;
    let volume = VolumeSample {
        total: DISK,
        free,
        available: free,
        mount: "/".into(),
    };
    let hour_rate = (n.free_gb[22] - n.free_gb[23]) * 1e9 / 3600.0;
    let filling = (hour_rate > 0.0 && free as f64 / hour_rate < 86_400.0).then(|| Filling {
        secs: free as f64 / hour_rate,
        rate: hour_rate,
        burst: false,
    });
    let disk_facts = DiskFacts {
        level: Some(if busy { DiskLevel::Low } else { DiskLevel::Ok }),
        warn: Some(69_000_000_000),
        floor: Some(34_000_000_000),
        emergency: Some(8_600_000_000),
        watch: true,
        ..DiskFacts::default()
    };

    let link = LinkSample {
        name: "en0".into(),
        kind: LinkKind::Ethernet,
        up: true,
        counts: true,
        rate_bps: Some(1_000_000_000),
        ..LinkSample::default()
    };
    let (rx, tx) = (n.net.now * LINK_BPS, n.net.now * LINK_BPS / 5.0);
    let rates = LinkRates {
        rx_bps: rx,
        tx_bps: tx,
        rx_pps: rx / 1_200.0,
        tx_pps: tx / 600.0,
        ..LinkRates::default()
    };
    let net = NetView {
        links: vec![LinkView {
            sample: link.clone(),
            rates: Some(rates),
        }],
        rates: Some(rates),
        window_ns: Some(1_000_000_000),
        tcp: None,
    };

    let (writer, gb_h) = n.writer;
    let writer_program = programs(scene).iter().find(|p| p.name == writer);
    let writer_scope = writer_program.map_or(ProcScope::Other, |p| p.owner.scope(scene));
    let writer_n = writer_program.map_or(1, |p| u32::try_from(p.cores.len()).unwrap_or(u32::MAX));
    let write_bps = gb_h * 1e9 / 3600.0;
    let writes_in = |k: Option<u64>| -> f64 {
        procs
            .iter()
            .filter(|p| match k {
                Some(k) => p.scope == RowScope::Tab(k),
                None => p.scope.in_aterm(),
            })
            .filter_map(|p| p.write_bps)
            .sum()
    };

    // The rows, in Resource::ALL order.
    let mut resources: [ResourceView; 7] = Default::default();
    let cpu_row = Resource::Cpu.index();
    resources[cpu_row] = view(
        n.cpu.now * n_cores,
        n_cores,
        ring(&n.cpu.run, n_cores, n.cpu.over),
        n.cpu.over.contains(&19),
        CapBasis::Fixed,
        None,
    );
    (resources[cpu_row].aterm, resources[cpu_row].tabs) = scope_figs(n, &n.cpu, n_cores, cores_of);

    let gpu_row = Resource::Gpu.index();
    resources[gpu_row] = view(
        n.gpu.now * 100.0,
        100.0,
        ring(&n.gpu.run, 100.0, &[]),
        false,
        CapBasis::Fixed,
        None,
    );
    (resources[gpu_row].aterm, resources[gpu_row].tabs) = scope_figs(n, &n.gpu, 100.0, cores_of);

    let mem_row = Resource::Memory.index();
    resources[mem_row] = view(
        n.mem.now * mem_total,
        mem_total,
        ring(&n.mem.run, mem_total, n.mem.over),
        n.mem.over.contains(&19),
        CapBasis::Fixed,
        None,
    );
    resources[mem_row].pressure = vec![kept_pressure(n); resources[mem_row].ring.len()].into();
    resources[mem_row].state = match n.pressure {
        Pressure::Warn => Some(State::Warn),
        Pressure::Critical => Some(State::Critical),
        _ => None,
    };
    (resources[mem_row].aterm, resources[mem_row].tabs) = scope_figs(n, &n.mem, mem_total, gib);

    let disk_row = Resource::Disk.index();
    let disk_run: Run = std::array::from_fn(|_| (volume.used() as f64 / DISK as f64) as f32);
    resources[disk_row] = view(
        volume.used() as f64,
        DISK as f64,
        ring(&disk_run, DISK as f64, &[]),
        false,
        CapBasis::Fixed,
        None,
    );
    resources[disk_row].state = disk_facts.level.and_then(DiskLevel::state);
    let flat_fig = |now: f64| fig(1.0, &flat(1.0), now, 1.0);
    resources[disk_row].aterm = flat_fig(writes_in(None));
    resources[disk_row].tabs = panes(scene)
        .map(|k| (k, flat_fig(writes_in(Some(k)))))
        .collect();

    let net_row = Resource::Network.index();
    resources[net_row] = view(
        rx,
        LINK_BPS,
        ring(&n.net.run, LINK_BPS, &[]),
        false,
        CapBasis::Link,
        Some(Series {
            now: Some(tx),
            capacity: Some(LINK_BPS),
            ring: ring(&n.net.run.map(|v| v / 5.0), LINK_BPS, &[]),
            binds: false,
        }),
    );
    let not_per_app = ScopeFig {
        missing: Some(NoFigure::NotPerApp),
        ..ScopeFig::default()
    };
    resources[net_row].aterm = not_per_app.clone();
    resources[net_row].tabs = panes(scene).map(|k| (k, not_per_app.clone())).collect();

    let tok_row = Resource::Tokens.index();
    let (tok_now, tok_peak) = n.tokens;
    let tok_run = flat((tok_now / tok_peak) as f32);
    resources[tok_row] = view(
        tok_now,
        tok_peak,
        ring(&tok_run, tok_peak, &[]),
        false,
        CapBasis::Peak,
        None,
    );
    let agents: Vec<u64> = programs(scene)
        .iter()
        .filter(|p| p.name == "claude")
        .filter_map(|p| match p.owner {
            Owner::Pane(k) => Some(k),
            _ => None,
        })
        .collect();
    let per_agent = tok_now / agents.len().max(1) as f64;
    resources[tok_row].aterm = flat_fig(tok_now);
    resources[tok_row].tabs = panes(scene)
        .map(|k| {
            let f = if agents.contains(&k) {
                flat_fig(per_agent)
            } else {
                ScopeFig {
                    missing: Some(NoFigure::NoAgent),
                    ..ScopeFig::default()
                }
            };
            (k, f)
        })
        .collect();

    let procs_row = Resource::Processes.index();
    let (mac_procs, yours) = n.counts;
    let system = LIMITS.system.unwrap_or(1) as f64;
    let per_user = LIMITS.per_user.unwrap_or(1) as f64;
    resources[procs_row] = view(
        mac_procs as f64,
        system,
        ring(&flat((mac_procs as f64 / system) as f32), system, &[]),
        false,
        CapBasis::Fixed,
        Some(Series {
            now: Some(yours as f64),
            capacity: Some(per_user),
            ring: ring(&n.procs, per_user, &[]),
            binds: false,
        }),
    );
    let count_fig = |k: Option<u64>| -> f64 {
        programs(scene)
            .iter()
            .filter(|p| match k {
                Some(k) => p.owner == Owner::Pane(k),
                None => !matches!(p.owner, Owner::Outside(_)),
            })
            .map(procs_of)
            .sum()
    };
    resources[procs_row].aterm = flat_fig(count_fig(None));
    resources[procs_row].tabs = panes(scene)
        .map(|k| (k, flat_fig(count_fig(Some(k)))))
        .collect();

    // The scopes as the lens and the words read them.
    let fig_of = |row: usize, k: u64| {
        resources[row]
            .tabs
            .iter()
            .find(|(key, _)| *key == k)
            .and_then(|(_, f)| f.now)
            .unwrap_or(0.0)
    };
    let top_of = |scope: &dyn Fn(RowScope) -> bool| {
        procs
            .iter()
            .filter(|p| scope(p.scope))
            .max_by(|a, b| a.cores.unwrap_or(0.0).total_cmp(&b.cores.unwrap_or(0.0)))
            .map(|p| (p.name.clone(), p.cores.unwrap_or(0.0)))
    };
    let rising = if busy { 0.9 } else { 1.0 };
    let scope_view = |cores: f64, bytes: f64, count: f64, gpu: f64, top, wait, writes| ScopeView {
        cores: trend(
            cores as f32,
            cores as f32 * rising,
            (cores as f32 * 0.97, cores as f32),
        ),
        footprint: Some(bytes as u64),
        footprint_gib: TrendView::flat((bytes / GIB) as f32),
        procs: count as u32,
        history: (0..HISTORY_LEN).map(|_| Some(cores as f32)).collect(),
        top,
        wait,
        write_bps: Some(writes),
        refused: 0,
        gpu: Some((gpu / 100.0) as f32),
    };
    let tab_views = panes(scene)
        .map(|k| {
            let (i, tab) = pane_tab(scene, k).unwrap_or((0, &BUSY_TABS[0]));
            let root = ["zsh", "claude"]
                .iter()
                .find_map(|name| {
                    procs
                        .iter()
                        .find(|p| p.scope == RowScope::Tab(k) && p.name == *name)
                })
                .map_or(0, |p| p.pid);
            let wait = (busy && k == 3).then_some(5.2_f32);
            TabView {
                key: k,
                label: format!("Tab {}", i + 1),
                program: tab.title.into(),
                shell_pid: root,
                scope: scope_view(
                    fig_of(cpu_row, k),
                    fig_of(mem_row, k),
                    fig_of(procs_row, k),
                    fig_of(gpu_row, k),
                    top_of(&|s| s == RowScope::Tab(k)),
                    wait.or(Some(0.0)),
                    writes_in(Some(k)),
                ),
            }
        })
        .collect();
    let aterm_now = |row: usize| resources[row].aterm.now.unwrap_or(0.0);
    let aterm = scope_view(
        aterm_now(cpu_row),
        aterm_now(mem_row),
        aterm_now(procs_row),
        aterm_now(gpu_row),
        top_of(&RowScope::in_aterm),
        Some(if busy { 5.4 } else { 0.0 }),
        writes_in(None),
    );
    let global = scope_view(
        n.cpu.now * n_cores,
        n.mem.now * mem_total,
        mac_procs as f64,
        n.gpu.now * 100.0,
        top_of(&|_| true),
        Some(if busy { 5.9 } else { 0.0 }),
        write_bps + if busy { 0.6e6 } else { 0.0 },
    );

    let mut top: Vec<&ProcView> = procs.iter().collect();
    top.sort_by(|a, b| b.cores.unwrap_or(0.0).total_cmp(&a.cores.unwrap_or(0.0)));
    let top = top
        .into_iter()
        .take(crate::model::TOP_PROCS)
        .map(|p| TopProc {
            name: p.name.clone(),
            cores: p.cores.unwrap_or(0.0),
            footprint: p.footprint,
            scope: match p.scope {
                RowScope::Tab(k) => tab_scope(scene, k),
                RowScope::SelfProc | RowScope::Helper => ProcScope::Aterm,
                _ => ProcScope::Other,
            },
        })
        .collect();
    let busiest = programs(scene)
        .iter()
        .max_by(|a, b| cores_of(a).total_cmp(&cores_of(b)))
        .map(|p| Busiest {
            name: p.name.into(),
            cores: cores_of(p) as f32,
            n: p.cores.len(),
            scope: Some(p.owner.scope(scene)),
            born_ms: None,
            unborn: p.cores.len(),
        });
    let aterm_cpu = aterm_now(cpu_row) as f32;
    let others: f32 = procs
        .iter()
        .filter(|p| p.scope == RowScope::Others)
        .filter_map(|p| p.cores)
        .sum();
    let global_cpu = (n.cpu.now * n_cores) as f32;

    let mut s = Snapshot {
        at_ns: 3_600_000_000_000,
        seq: 360,
        cpu: Some(cpu),
        gpu: Some(gpu),
        mem: Some(mem),
        volume: Some(volume),
        net: Some(net),
        global,
        aterm: Some(aterm),
        tabs: tab_views,
        top,
        at_unix_ms: AT,
        resources,
        free_history: free_history(&n.free_gb),
        facts: RowFacts {
            load1: Some(n.load[0]),
            busiest,
            swap_rate: Some(swap_bps),
            gpu: Some(gpu_sample),
            gpu_resets: None,
            disk: Some(disk_facts),
            disk_rates: Rates {
                rate5: Some(hour_rate),
                rate10: Some(hour_rate),
                rate60: Some(hour_rate),
            },
            filling,
            writers: vec![Writer {
                name: writer.into(),
                bytes_per_s: write_bps,
                scope: writer_scope,
                n: writer_n,
                bytes: write_bps as u64,
            }],
            // A whole hour read, at the mockup's rate: `per_hour` gives its GB an hour back.
            hour_writers: vec![ProgramHour {
                name: writer.into(),
                scope: writer_scope,
                bytes: (gb_h * 1e9) as u64,
                n_max: writer_n,
            }],
            writes_covered_s: 3600.0,
            link: Some(link),
            net_read_s: 3_600,
            tokens_in: Some(tok_now * 40.0),
            agents: agents.len(),
            tokens_read_s: 7 * 86_400,
            plan: Some(PlanFact {
                window_word: "weekly".into(),
                used_x10: n.plan.0[6],
                read_at: AT - 4 * 60_000,
                account: AccountIx(0),
            }),
            plan_week: Some(plan_run(n)),
            pid_counts: (Some(mac_procs), Some(yours)),
            proc_limits: LIMITS,
        },
        procs_window_ns: Some(1_000_000_000),
        cores_window_ns: Some(1_000_000_000),
        split: Some(Split {
            global: global_cpu,
            aterm: aterm_cpu,
            others,
            services: (global_cpu - aterm_cpu - others).max(0.0),
            unaccounted: 0.0,
            unread: 0,
        }),
        procs,
        load: Some(n.load),
        avail: Some(((128.0 - used - compressed) * GIB) as u64),
        self_pid: SELF_PID,
        ..Snapshot::default()
    };
    s.set_jobs(Some(Arc::new(jobs)));
    s
}

/// Every optional reading absent: the window's panes known, nothing read yet, under an hour of
/// free history (restored from an earlier run), GPU and Network kept per app by nobody.
fn absent() -> Snapshot {
    let scene = DeckScene::Absent;
    let mut resources: [ResourceView; 7] = Default::default();
    for res in Resource::ALL {
        let missing = match res {
            Resource::Gpu | Resource::Network => Some(NoFigure::NotPerApp),
            Resource::Tokens => Some(NoFigure::NoAgent),
            _ => None,
        };
        let fig = ScopeFig {
            missing,
            ..ScopeFig::default()
        };
        let v = &mut resources[res.index()];
        v.aterm = fig.clone();
        v.tabs = panes(scene).map(|k| (k, fig.clone())).collect();
        v.basis = match res {
            Resource::Network | Resource::Tokens => CapBasis::Learning,
            _ => CapBasis::Fixed,
        };
    }
    let mut free_history = FreeHistory::new();
    for i in 0..4 {
        free_history.note_on("/", DISK, AT - (3 - i) * 600_000, 880_000_000_000);
    }
    Snapshot {
        seq: 1,
        at_unix_ms: AT,
        resources,
        free_history,
        self_pid: SELF_PID,
        ..Snapshot::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row's units: its programs, or for all tabs each other tab as one and aterm.
    fn rows(scene: DeckScene) -> [Vec<(String, f64, f64)>; 3] {
        let front = tabs(scene)[front(scene)].panes;
        let mut out: [Vec<(String, f64, f64)>; 3] = Default::default();
        for p in programs(scene) {
            let (row, name) = match p.owner {
                Owner::Outside(_) => (0, p.name.to_owned()),
                Owner::Pane(k) if front.contains(&k) => (2, p.name.to_owned()),
                Owner::Pane(k) => (
                    1,
                    format!("Tab {}", pane_tab(scene, k).map_or(0, |t| t.0 + 1)),
                ),
                Owner::Aterm => (1, "aterm".to_owned()),
            };
            match out[row].iter_mut().find(|u| u.0 == name) {
                Some(u) => {
                    u.1 += cores_of(p);
                    u.2 += p.gib;
                }
                None => out[row].push((name, cores_of(p), p.gib)),
            }
        }
        out
    }

    /// Each scope's figure in `res`: the Mac's, all tabs', this tab's (its panes summed).
    fn figures(s: &Snapshot, scene: DeckScene, res: Resource) -> [Option<f64>; 3] {
        let v = &s.resources[res.index()];
        let front = tabs(scene)[front(scene)].panes;
        let tab = v
            .tabs
            .iter()
            .filter(|(k, _)| front.contains(k))
            .map(|(_, f)| f.now)
            .try_fold(0.0, |sum, now| now.map(|n| sum + n));
        [v.now, v.aterm.now, tab]
    }

    /// gen.py's `check_shares` carried over: this tab within all tabs within the Mac for every
    /// row, each row's programs within its scope's share of the CPU and of memory, and every row
    /// in the rank the deck's one rule gives.
    #[test]
    fn fixtures_hold_their_shares() {
        for scene in [DeckScene::Busy, DeckScene::Calm] {
            let s = snapshot(scene);
            let same_unit = Resource::ALL
                .into_iter()
                .filter(|&r| r.scope_shares_capacity() || r == Resource::Gpu);
            for res in same_unit {
                if let [Some(mac), Some(tabs), Some(tab)] = figures(&s, scene, res) {
                    assert!(
                        tab <= tabs + 1e-9 && tabs <= mac + 1e-9,
                        "{scene:?} {res:?}: this tab {tab} all tabs {tabs} Mac {mac}"
                    );
                }
            }
            let units = rows(scene);
            for (res, unit) in [(Resource::Cpu, 1.0), (Resource::Memory, GIB)] {
                let [mac, tabs, tab] = figures(&s, scene, res).map(|f| f.unwrap_or(0.0) / unit);
                let cap = s.resources[res.index()].capacity.unwrap_or(0.0) / unit;
                let tol = cap * 0.01;
                for (row, room) in [mac - tabs, tabs - tab, tab].into_iter().enumerate() {
                    let used: f64 = units[row]
                        .iter()
                        .map(|u| if res == Resource::Cpu { u.1 } else { u.2 })
                        .sum();
                    assert!(
                        used <= room + tol,
                        "{scene:?} {res:?} row {row}: {:?} use {used:.2}, over {room:.2}",
                        units[row]
                    );
                }
            }
            let share = |res: Resource| s.resources[res.index()].heat_now.unwrap_or(0.0);
            let by_cpu = share(Resource::Cpu) >= share(Resource::Memory);
            let key = |u: &(String, f64, f64)| if by_cpu { u.1 / 18.0 } else { u.2 / 128.0 };
            for row in &units {
                for pair in row.windows(2) {
                    assert!(
                        key(&pair[0]) >= key(&pair[1]),
                        "{scene:?}: {:?} ranks after {:?}",
                        pair[0],
                        pair[1]
                    );
                }
            }
            let mut mac: Vec<&(String, f64, f64)> = units.iter().flatten().collect();
            mac.sort_by(|a, b| key(b).total_cmp(&key(a)));
            let lead: Vec<&str> = mac.iter().take(2).map(|u| u.0.as_str()).collect();
            let want = match scene {
                DeckScene::Busy => ["rustc", "Spotlight"],
                _ => ["Safari", "Tab 1"],
            };
            assert_eq!(lead, want, "{scene:?}: the whole Mac's top");
        }
    }

    #[test]
    fn the_scenes_draw_the_mockups_figures() {
        let pct = |s: &Snapshot, res: Resource| {
            let v = &s.resources[res.index()];
            (v.binding().now.unwrap_or(0.0) / v.binding().capacity.unwrap_or(1.0) * 100.0).round()
        };
        let busy = snapshot(DeckScene::Busy);
        let calm = snapshot(DeckScene::Calm);
        for (s, want) in [
            (&busy, [99.0, 12.0, 88.0, 98.0, 6.0]),
            (&calm, [6.0, 3.0, 24.0, 56.0, 4.0]),
        ] {
            let got = [
                Resource::Cpu,
                Resource::Gpu,
                Resource::Memory,
                Resource::Disk,
                Resource::Processes,
            ]
            .map(|r| pct(s, r));
            assert_eq!(got, want);
        }
        for (s, level) in [(&busy, Pressure::Warn), (&calm, Pressure::Normal)] {
            let mem = &s.resources[Resource::Memory.index()];
            assert_eq!(mem.pressure.len(), mem.ring.len(), "kept cell for cell");
            assert!(mem.pressure.iter().all(|&p| p == Some(level)));
            let m = s.mem.as_ref().expect("memory");
            assert_eq!(m.pressure_history, vec![Some(level); m.history.len()]);
        }
        let filling = busy.facts.filling.expect("busy fills");
        assert_eq!((filling.secs / 60.0).round(), 34.0);
        assert!(calm.facts.filling.is_none());
        let counts = |s: &Snapshot| {
            let front = &s
                .tabs
                .iter()
                .filter(|t| t.key == 3 || t.key == 5)
                .map(|t| t.scope.procs)
                .sum::<u32>();
            (
                s.facts.pid_counts.0,
                s.aterm.as_ref().map(|a| a.procs),
                *front,
            )
        };
        assert_eq!(counts(&busy), (Some(897), Some(21), 14));
        let jobs = busy.jobs.as_deref().expect("busy's jobs");
        let verdicts: Vec<(u64, &str, Verdict)> = jobs
            .rows
            .iter()
            .map(|r| (r.tab, r.exe.as_str(), r.verdict))
            .collect();
        assert_eq!(
            verdicts,
            [
                (1, "sudo", Verdict::WaitingYou),
                (3, "cargo", Verdict::Fine),
                (5, "node", Verdict::Stuck)
            ]
        );
        for s in [&busy, &calm] {
            for p in &s.procs {
                let from_aterm = p.ppid == SELF_PID
                    || s.procs
                        .iter()
                        .any(|q| q.pid == p.ppid && q.scope.in_aterm());
                assert_eq!(
                    from_aterm,
                    p.scope.in_aterm() && p.pid != SELF_PID,
                    "{} {:?} under {}",
                    p.name,
                    p.scope,
                    p.ppid
                );
            }
        }
        let stuck = &jobs.rows[2];
        assert!(
            busy.procs
                .iter()
                .any(|p| p.name == "node" && p.pgid == stuck.pgid)
        );
        assert_eq!(
            busy.resources[Resource::Processes.index()].state,
            Some(State::Stuck)
        );
        let plan = busy.facts.plan_week.as_ref().expect("a week");
        assert_eq!(plan.week.days.len(), 8, "the reset day keeps two rows");
        let top = &busy.facts.hour_writers[0];
        let gb_h = top
            .per_hour(busy.facts.writes_covered_s)
            .map(|b| (b / 1e9).round());
        assert_eq!(
            (top.name.as_str(), gb_h),
            ("rustc", Some(68.0)),
            "rustc writes 68 GB/h"
        );
    }

    #[test]
    fn absent_reads_nothing() {
        let s = snapshot(DeckScene::Absent);
        assert!(s.cpu.is_none() && s.gpu.is_none() && s.mem.is_none() && s.volume.is_none());
        assert!(s.aterm.is_none() && s.tabs.is_empty() && s.jobs.is_none());
        assert!(s.facts.plan_week.is_none() && s.facts.filling.is_none());
        assert!(s.facts.hour_writers.is_empty() && s.facts.writes_covered_s == 0.0);
        assert!(s.free_history.len() < 6, "under an hour of free history");
        for v in &s.resources {
            assert!(v.now.is_none() && v.ring.is_empty() && v.pressure.is_empty());
            assert!(
                v.tabs
                    .iter()
                    .all(|(_, f)| f.now.is_none() && f.run.is_empty())
            );
        }
        let gpu = &s.resources[Resource::Gpu.index()];
        assert_eq!(gpu.aterm.missing, Some(NoFigure::NotPerApp));
    }
}
