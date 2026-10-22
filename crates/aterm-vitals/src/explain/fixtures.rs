// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scenes the split's tests and the scope balance's conformance
//! (`tests/conformance_scope_balance.rs`) read through the real
//! [`Vitals::absorb`](crate::Vitals::absorb): one window of work, as the host
//! reads it — a split tick, a sweep [`Scene::sweep_ms`] before the next one,
//! and that next split tick [`Scene::window_ms`] after the first. Each sweep
//! lists the processes newest first, as the kernel does (`proc_listallpids`),
//! so a coalition's first-swept member is seldom its lead.
//!
//! Figures are millicores over the window. A coalition's counters and a
//! process's CPU time grow by exactly its figure, so a split of the scene
//! reads the figures back with no rounding.

use super::{CoalLane, CoalSample, CoalUsage, LeadIdentity, TimelineSample};
use crate::model::{DelegatedJob, ProcSample, ProcState, Sample, TabRoot, Ticks};

/// When a coalition or a process is there: at both reads, or made, or gone,
/// inside the window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Span {
    /// At both split ticks.
    #[default]
    Whole,
    /// Made inside the window: absent at the first tick.
    Born,
    /// Gone inside the window: absent at the second tick.
    Gone,
}

/// One resource coalition of a scene.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Coal {
    /// Its id.
    pub id: u64,
    /// Its CPU over the window, millicores.
    pub mc: u64,
    /// Of that, at maintenance or background QoS.
    pub deferred_mc: u64,
    /// Of the deferred work, at maintenance QoS (the rest at background).
    pub maint_mc: u64,
    /// Bytes per second it read from storage over the window.
    pub read_bps: u64,
    /// When it is listed.
    pub span: Span,
    /// Its tasks that had started, at both ticks (1 unless set).
    pub started: u64,
    /// Its tasks that had exited, at both ticks.
    pub exited: u64,
}

impl Coal {
    /// Coalition `id`, listed at both ticks, at `mc` millicores.
    #[must_use]
    pub fn new(id: u64, mc: u64) -> Self {
        Self {
            id,
            mc,
            started: 1,
            ..Self::default()
        }
    }

    /// The same, listed only from the second tick.
    #[must_use]
    pub fn born(self) -> Self {
        Self {
            span: Span::Born,
            ..self
        }
    }

    /// The same, listed only at the first tick (reaped inside the window).
    #[must_use]
    pub fn gone(self) -> Self {
        Self {
            span: Span::Gone,
            ..self
        }
    }
}

/// One process of a scene.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Proc {
    /// Its pid.
    pub pid: u32,
    /// Its parent.
    pub ppid: u32,
    /// Its user.
    pub uid: u32,
    /// Its resource coalition.
    pub coalition: u64,
    /// Its birth, unix ms.
    pub birth_ms: i64,
    /// Its name.
    pub name: String,
    /// Its CPU over the window, millicores.
    pub mc: u64,
    /// Its CPU over the last sweep's window, millicores (its figure, unless
    /// a build is starting).
    pub fast_mc: u64,
    /// When it is swept.
    pub span: Span,
}

impl Proc {
    /// Process `pid` (child of `ppid`, user `uid`, in coalition `coalition`)
    /// named `name`, at `mc` millicores over both windows, born at
    /// `birth_ms`.
    #[must_use]
    pub fn new(
        pid: u32,
        ppid: u32,
        uid: u32,
        coalition: u64,
        birth_ms: i64,
        name: &str,
        mc: u64,
    ) -> Self {
        Self {
            pid,
            ppid,
            uid,
            coalition,
            birth_ms,
            name: name.to_owned(),
            mc,
            fast_mc: mc,
            span: Span::Whole,
        }
    }

    /// The same at `fast_mc` over the last sweep's window.
    #[must_use]
    pub fn fast(self, fast_mc: u64) -> Self {
        Self { fast_mc, ..self }
    }

    /// The same, swept only at the first tick and the sweep between (it
    /// exits inside the window).
    #[must_use]
    pub fn gone(self) -> Self {
        Self {
            span: Span::Gone,
            ..self
        }
    }

    /// The same, swept only from the sweep between the ticks (born inside
    /// the window).
    #[must_use]
    pub fn born(self) -> Self {
        Self {
            span: Span::Born,
            ..self
        }
    }
}

/// One window of work.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scene {
    /// From one split tick to the next, ms.
    pub window_ms: u64,
    /// The last sweep's window before the second tick, ms.
    pub sweep_ms: u64,
    /// The logical cores.
    pub cores: usize,
    /// The cores' busy figure over the window, millicores.
    pub busy_mc: u64,
    /// This aterm's pid.
    pub self_pid: u32,
    /// This aterm's user.
    pub self_uid: u32,
    /// Every tab.
    pub tabs: Vec<TabRoot>,
    /// The delegated jobs.
    pub delegated: Vec<DelegatedJob>,
    /// The coalitions that answer.
    pub coalitions: Vec<Coal>,
    /// Listed coalitions that answer nothing (coalition 2).
    pub unanswered: u32,
    /// The processes, in the order they were made: each sweep lists them
    /// last first.
    pub procs: Vec<Proc>,
    /// The identities the host read of the leads.
    pub leads: Vec<LeadIdentity>,
    /// The machine's timeline.
    pub timeline: Option<TimelineSample>,
    /// The lane.
    pub lane: CoalLane,
}

/// The first tick's monotonic stamp, ms.
const T0_MS: u64 = 100_000;
/// The wall clock at the monotonic clock's origin, unix ms: a 5 s scene's
/// second tick reads RD's 10:32 moment, 17:32:41.200Z.
const WALL_MS: i64 = 1_792_517_456_200;
/// Every core's total ticks at the first tick (one tick a ms).
const TICKS0: u64 = 1_000_000;
/// A counter's value at the first tick, ns.
const CPU0_NS: u64 = 1_000_000_000_000;

impl Scene {
    /// The scene's three readings: the first split tick, the sweep between,
    /// the second split tick.
    #[must_use]
    pub fn samples(&self) -> [Sample; 3] {
        let w = self.window_ms;
        [
            self.reading(0, true),
            self.reading(w - self.sweep_ms, false),
            self.reading(w, true),
        ]
    }

    /// The reading `at_ms` into the window: a split tick reads the
    /// coalitions, each process's coalition and birth, and the leads.
    fn reading(&self, at_ms: u64, split: bool) -> Sample {
        let w = self.window_ms;
        let end = at_ms == w;
        let there = |span: Span| match span {
            Span::Whole => true,
            Span::Born => at_ms > 0,
            Span::Gone => !end,
        };
        // `at_ms` of `mc` millicores over the window, ns: the second tick
        // reads all of it, the sweep its figure less the last sweep's.
        let grown = |mc: u64, fast: u64| match at_ms {
            0 => 0,
            a if a == w => mc * w * 1000,
            _ => mc * w * 1000 - fast * self.sweep_ms * 1000,
        };
        let busy = self.busy_mc * w / 1000;
        let cores = (0..self.cores)
            .map(|i| {
                let share =
                    busy / self.cores as u64 + u64::from((i as u64) < busy % self.cores as u64);
                Ticks {
                    busy: TICKS0 / 2 + share * at_ms / w,
                    total: TICKS0 + at_ms,
                }
            })
            .collect();
        let procs = self
            .procs
            .iter()
            .rev()
            .filter(|p| there(p.span))
            .map(|p| ProcSample {
                pid: p.pid,
                start: Some(u64::try_from(p.birth_ms).unwrap_or(0)),
                ppid: p.ppid,
                cpu_ns: Some(CPU0_NS + grown(p.mc, p.fast_mc)),
                footprint: Some(16 << 20),
                name: p.name.clone(),
                uid: Some(p.uid),
                pgid: p.pid,
                state: ProcState::Run,
                coalition: split.then_some(p.coalition),
                birth_ms: split.then_some(p.birth_ms),
                ..ProcSample::default()
            })
            .collect();
        let coalitions = split.then(|| {
            let rows: Vec<CoalUsage> = self
                .coalitions
                .iter()
                .filter(|c| there(c.span))
                .map(|c| {
                    let base = if c.span == Span::Born { 0 } else { CPU0_NS };
                    let read = if c.span == Span::Born { 0 } else { 1 << 30 };
                    let mut eqos_ns = [0; super::QOS_CLASSES];
                    eqos_ns[1] = c.maint_mc * at_ms * 1000;
                    eqos_ns[2] = base / 2 + (c.deferred_mc - c.maint_mc) * at_ms * 1000;
                    eqos_ns[4] = base / 2 + (c.mc - c.deferred_mc) * at_ms * 1000;
                    CoalUsage {
                        id: c.id,
                        tasks_started: c.started,
                        tasks_exited: c.exited,
                        cpu_ns: base + c.mc * at_ms * 1000,
                        bytes_read: read + c.read_bps * at_ms / 1000,
                        eqos_ns,
                        footprint: Some(64 << 20),
                    }
                })
                .collect();
            CoalSample {
                listed: u32::try_from(rows.len()).unwrap_or(u32::MAX) + self.unanswered,
                // The unanswered coalition 2 is below every row.
                newest: rows.iter().map(|r| r.id).max(),
                truncated: false,
                rows,
            }
        });
        let mono_ms = T0_MS + at_ms;
        Sample {
            at_ns: mono_ms * 1_000_000,
            cores: Some(cores),
            procs: Some(procs),
            self_pid: self.self_pid,
            self_uid: Some(self.self_uid),
            tabs: self.tabs.clone(),
            delegated: self.delegated.clone(),
            at_unix_ms: WALL_MS + i64::try_from(mono_ms).unwrap_or(0),
            mono_ms,
            cadence_ms: 1000,
            coalitions,
            leads: if split {
                self.leads.clone()
            } else {
                Vec::new()
            },
            timeline: if split { self.timeline.clone() } else { None },
            coal_lane: self.lane,
            ..Sample::default()
        }
    }
}

/// A tab with key `key`, labelled `Tab <key>`, whose shell is `shell_pid`.
#[must_use]
pub fn tab(key: u64, shell_pid: u32) -> TabRoot {
    TabRoot {
        key,
        shell_pid,
        label: format!("Tab {key}"),
        program: String::new(),
    }
}

/// RD's 10:32 moment (constructed there, `At a glance`): 3.30 of 18 cores
/// busy over a 5 s window ending 17:32:41.200Z — tab 1 0.20, the rest of
/// aterm 0.20 (aterm's coalition 1342 holds 0.40), others 0.73
/// (`siriactionsd` 0.66, Finder 0.07), services 2.17 (`mds` 1.29,
/// `contextstored` 0.46, `mds_stores` 0.13, kernel and launchd 0.17, `logd`
/// 0.12), nothing unaccounted; coalition 2 answers nothing. Finder's 0.07
/// and `logd`'s 0.12 stand in for RD's 570 small coalitions (its 120/70
/// split, illustrative there). `mds` reads 22.9 MB/s and runs 722 ‰
/// deferred (931 mc: 280 mc at maintenance, 651 mc at background), as its
/// lifetime did at 10:55 (1,275 s: 277 s maintenance, 644 s background;
/// `brief-macos-why.md`);
/// `mds_stores` runs 723 ‰ (constructed), at background. Before the
/// window, `mds`, `contextstored` and `mds_stores` had started 2 tasks and
/// seen 1 exit each, aterm 6,725 and 6,705, the kernel 190 and 188
/// (`probes/why-out-1.txt`). The kernel booted at 16:06:13Z and launchd was
/// born a second later; the Mac slept at 16:26:38Z, woke on a trackpad touch
/// at 17:28:33Z, and userspace started at 17:28:38Z (`brief-macos-why.md`,
/// `probes/pidwrap-out-1.txt`).
#[must_use]
pub fn spotlight_1032() -> Scene {
    // The births of RD's `who` rows (17:28:38–17:31:55Z).
    let start = 1_792_517_318_000;
    let launchd = 1_792_512_374_000;
    Scene {
        window_ms: 5000,
        sweep_ms: 1000,
        cores: 18,
        busy_mc: 3300,
        self_pid: 1187,
        self_uid: 501,
        tabs: vec![tab(1, 1200)],
        coalitions: vec![
            Coal {
                started: 6725,
                exited: 6705,
                ..Coal::new(1342, 400)
            },
            Coal::new(1092, 660),
            Coal::new(1100, 70),
            Coal {
                deferred_mc: 931,
                maint_mc: 280,
                read_bps: 22_900_000,
                started: 2,
                exited: 1,
                ..Coal::new(307, 1290)
            },
            Coal {
                started: 2,
                exited: 1,
                ..Coal::new(327, 460)
            },
            Coal {
                deferred_mc: 94,
                started: 2,
                exited: 1,
                ..Coal::new(491, 130)
            },
            Coal::new(247, 120),
            Coal {
                started: 190,
                exited: 188,
                ..Coal::new(1, 170)
            },
        ],
        unanswered: 1,
        procs: vec![
            Proc::new(1, 0, 0, 1, launchd, "launchd", 170),
            Proc::new(357, 1, 0, 247, start, "logd", 120),
            Proc::new(387, 1, 0, 307, start, "mds", 1290),
            Proc::new(397, 1, 0, 327, start, "contextstored", 460),
            Proc::new(533, 1, 308, 491, start + 1_000, "mds_stores", 130),
            Proc::new(916, 1, 501, 1092, start + 7_000, "siriactionsd", 660),
            Proc::new(940, 1, 501, 1100, start + 9_000, "Finder", 70),
            Proc::new(1187, 1, 501, 1342, start + 197_000, "aterm", 150),
            Proc::new(1190, 1187, 501, 1342, start + 198_000, "aterm-link", 50),
            Proc::new(1200, 1187, 501, 1342, start + 199_000, "zsh", 0),
            Proc::new(1210, 1200, 501, 1342, start + 200_000, "claude", 200),
        ],
        timeline: Some(TimelineSample {
            boot_ms: launchd - 1_000,
            sleep_ms: Some(1_792_513_598_000),
            wake_ms: Some(start - 5_000),
            wake_reason: Some("RTP.multi-touch".into()),
        }),
        lane: CoalLane::On,
        ..Scene::default()
    }
}

/// SA's 14:21 window (§7.7, one 10 s window, two tabs building): 10.00 of 10
/// cores busy; aterm 9.76 over TWO coalitions, 55990 (this instance and tab
/// 2) and 55954 (the instance before the last live upgrade, which still
/// holds the adopted tab 1); others 0.03, services 0.18, unaccounted 0.03.
/// Live, tab 1 reads 3.35, tab 2 0.08 and aterm itself 0.02: the rest of
/// each tab's figure is work that finished between sweeps. The 9.76's split
/// between the two coalitions, 4.12 and 5.64, is constructed: SA gives
/// aterm's whole and each tab's (4.13 and 5.64), not each coalition's.
#[must_use]
pub fn two_coalitions_1421() -> Scene {
    let start = 1_792_500_000_000;
    Scene {
        window_ms: 10_000,
        sweep_ms: 1000,
        cores: 10,
        busy_mc: 10_000,
        self_pid: 7000,
        self_uid: 501,
        tabs: vec![tab(1, 6100), tab(2, 7100)],
        coalitions: vec![
            Coal::new(55954, 4120),
            Coal::new(55990, 5640),
            Coal::new(60001, 30),
            Coal::new(393, 120),
            Coal::new(1, 60),
        ],
        procs: vec![
            Proc::new(1, 0, 0, 1, start, "launchd", 60),
            Proc::new(430, 1, 88, 393, start + 1_000, "WindowServer", 120),
            Proc::new(5000, 1, 501, 60001, start + 3_000, "Notes", 30),
            Proc::new(6100, 1, 501, 55954, start + 50_000, "zsh", 0),
            Proc::new(6110, 6100, 501, 55954, start + 51_000, "claude", 3350),
            Proc::new(7000, 1, 501, 55990, start + 90_000, "aterm", 20),
            Proc::new(7100, 7000, 501, 55990, start + 91_000, "zsh", 0),
            Proc::new(7110, 7100, 501, 55990, start + 92_000, "claude", 80),
        ],
        lane: CoalLane::On,
        ..Scene::default()
    }
}

/// The 10:32 moment with tab 1's live figure read past aterm's coalition
/// (constructed): the sweep read `claude` at 0.45 over the window, after
/// coalition 1342's counter, which still reads 0.40, and the cores 3.30.
#[must_use]
pub fn tab_ahead_1032() -> Scene {
    let mut scene = spotlight_1032();
    for p in &mut scene.procs {
        if p.pid == 1210 {
            *p = Proc::new(p.pid, p.ppid, p.uid, p.coalition, p.birth_ms, "claude", 450);
        }
    }
    scene
}

/// The 10:32 moment with a coalition reaped inside the window (80000, 0.25)
/// and one made inside it (90000, `Photos`, 0.30, numbered after every
/// other): 3.85 busy, the reaped 0.25 in no part.
#[must_use]
pub fn reaped_1032() -> Scene {
    let mut scene = spotlight_1032();
    scene.busy_mc += 300 + 250;
    scene.coalitions.push(Coal::new(80_000, 250).gone());
    scene.coalitions.push(Coal::new(90_000, 300).born());
    scene
        .procs
        .push(Proc::new(3100, 1, 501, 90_000, 1_792_517_560_000, "Photos", 300).born());
    scene
}

/// The 10:32 moment with tab 1's delegated job (`cargo`, at `mc`) under an
/// app server (at `launcher_mc`) in the user's coalition 4000, which reads
/// 0.60 in all.
#[must_use]
pub fn delegated_1032(mc: u64, launcher_mc: u64) -> Scene {
    let born = 1_792_517_500_000;
    let mut scene = spotlight_1032();
    scene.busy_mc += 600;
    scene.coalitions.push(Coal::new(4000, 600));
    scene.procs.push(Proc::new(
        4001,
        1,
        501,
        4000,
        born,
        "app-server",
        launcher_mc,
    ));
    scene
        .procs
        .push(Proc::new(4010, 4001, 501, 4000, born + 1, "cargo", mc));
    let key = crate::delegated::ProcessKey {
        pid: 4010,
        start: u64::try_from(born + 1).unwrap_or(0),
    };
    scene.delegated.push(DelegatedJob {
        root: key,
        tab: Some(1),
        members: vec![key],
    });
    scene
}

/// The 10:32 moment with tab 1's delegated job (`cargo`, at `mc`) detached
/// from the tab (its parent is launchd) but still in aterm's coalition 1342,
/// which a process a tab starts inherits.
#[must_use]
pub fn delegated_in_aterm_1032(mc: u64) -> Scene {
    let born = 1_792_517_520_000;
    let mut scene = spotlight_1032();
    scene.busy_mc += mc;
    for c in &mut scene.coalitions {
        if c.id == 1342 {
            c.mc += mc;
        }
    }
    scene
        .procs
        .push(Proc::new(1300, 1, 501, 1342, born, "cargo", mc));
    let key = crate::delegated::ProcessKey {
        pid: 1300,
        start: u64::try_from(born).unwrap_or(0),
    };
    scene.delegated.push(DelegatedJob {
        root: key,
        tab: Some(1),
        members: vec![key],
    });
    scene
}

/// The 10:32 moment as tab 1's build starts: its `claude` 1.00 over the 5 s
/// window but 3.00 over the last 1 s sweep, so aterm's coalition reads 1.20
/// and the cores 4.10.
#[must_use]
pub fn build_start_1032() -> Scene {
    let mut scene = spotlight_1032();
    scene.busy_mc += 800;
    for c in &mut scene.coalitions {
        if c.id == 1342 {
            c.mc += 800;
        }
    }
    for p in &mut scene.procs {
        if p.pid == 1210 {
            *p = Proc::new(
                p.pid,
                p.ppid,
                p.uid,
                p.coalition,
                p.birth_ms,
                "claude",
                1000,
            )
            .fast(3000);
        }
    }
    scene
}
