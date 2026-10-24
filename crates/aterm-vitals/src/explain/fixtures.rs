// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test scenes fed through [`Vitals::absorb`](crate::Vitals::absorb).

use super::{CoalLane, CoalSample, CoalUsage, LeadIdentity, TimelineSample, Voucher};
use crate::model::{
    DelegatedJob, MemCounters, MemSample, Pressure, ProcSample, ProcState, Sample, TabRoot,
    Thermal, Ticks,
};

/// When a coalition or a process is there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Span {
    /// At both split ticks.
    #[default]
    Whole,
    /// Made inside the window.
    Born,
    /// Gone inside the window.
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
    /// Excess of QoS bucket sum over CPU time in the window (millicores).
    pub qos_over_mc: u64,
    /// Its footprint, bytes (64 MiB unless set).
    pub footprint: Option<u64>,
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
    /// Its CPU over the last sweep's window, millicores (its figure, unless a build is starting).
    pub fast_mc: u64,
    /// Its footprint, bytes (16 MiB unless set).
    pub footprint: u64,
    /// When it is swept.
    pub span: Span,
}

impl Proc {
    /// Process `pid` (child of `ppid`, user `uid`, in coalition `coalition`) named `name`, at `mc` millicores over both windows, born at `birth_ms`.
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
            footprint: 16 << 20,
            span: Span::Whole,
        }
    }

    /// The same with a footprint of `footprint` bytes.
    #[must_use]
    pub fn holding(self, footprint: u64) -> Self {
        Self { footprint, ..self }
    }

    /// The same at `fast_mc` over the last sweep's window.
    #[must_use]
    pub fn fast(self, fast_mc: u64) -> Self {
        Self { fast_mc, ..self }
    }

    /// The same, swept only at the first tick and the sweep between (it exits inside the window).
    #[must_use]
    pub fn gone(self) -> Self {
        Self {
            span: Span::Gone,
            ..self
        }
    }

    /// The same, swept only from the sweep between the ticks (born inside the window).
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
    /// The processes, in the order they were made.
    pub procs: Vec<Proc>,
    /// The identities the host read of the leads.
    pub leads: Vec<LeadIdentity>,
    /// The machine's timeline.
    pub timeline: Option<TimelineSample>,
    /// The lane.
    pub lane: CoalLane,
    /// The second split tick's wall clock, unix ms.
    pub at_ms: Option<i64>,
    /// Memory, at every reading.
    pub mem: Option<MemSample>,
    /// The thermal state, at every reading.
    pub thermal: Option<Thermal>,
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
    /// The scene's three readings.
    #[must_use]
    pub fn samples(&self) -> [Sample; 3] {
        let w = self.window_ms;
        [
            self.reading(0, true),
            self.reading(w - self.sweep_ms, false),
            self.reading(w, true),
        ]
    }

    /// The same work held over `windows` windows.
    #[must_use]
    pub fn series(&self, windows: u64) -> Vec<Sample> {
        let w = self.window_ms;
        std::iter::once(self.reading(0, true))
            .chain((1..=windows).flat_map(|k| {
                [
                    self.reading(k * w - self.sweep_ms, false),
                    self.reading(k * w, true),
                ]
            }))
            .collect()
    }

    /// The reading `at_ms` from the first split tick: a split tick (at a
    /// whole number of windows) reads the coalitions, each process's
    /// coalition, and the leads; every reading reads each process's birth,
    /// as each of the host's whole readings does. The host's dev seam
    /// (`ATERM_DEV_WHY_FIXTURE`) asks it at any `at_ms`, a split tick where
    /// the host's own cadence makes one: every counter grows at its figure,
    /// so a scene whose processes keep one rate ([`Proc::fast`] unset) reads
    /// its figures over any window.
    #[must_use]
    pub fn reading(&self, at_ms: u64, split: bool) -> Sample {
        let w = self.window_ms;
        let there = |span: Span| match span {
            Span::Whole => true,
            Span::Born => at_ms > 0,
            Span::Gone => at_ms < w,
        };
        let grown = |mc: u64, fast: u64| {
            if at_ms.is_multiple_of(w) {
                mc * at_ms * 1000
            } else {
                mc * (at_ms + self.sweep_ms) * 1000 - fast * self.sweep_ms * 1000
            }
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
                footprint: Some(p.footprint),
                name: p.name.clone(),
                uid: Some(p.uid),
                pgid: p.pid,
                state: ProcState::Run,
                coalition: split.then_some(p.coalition),
                birth_ms: Some(p.birth_ms),
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
                    eqos_ns[4] = base / 2 + (c.mc - c.deferred_mc + c.qos_over_mc) * at_ms * 1000;
                    CoalUsage {
                        id: c.id,
                        tasks_started: c.started,
                        tasks_exited: c.exited,
                        cpu_ns: base + c.mc * at_ms * 1000,
                        bytes_read: read + c.read_bps * at_ms / 1000,
                        eqos_ns,
                        footprint: Some(c.footprint.unwrap_or(64 << 20)),
                    }
                })
                .collect();
            CoalSample {
                listed: u32::try_from(rows.len()).unwrap_or(u32::MAX) + self.unanswered,
                newest: rows.iter().map(|r| r.id).max(),
                truncated: false,
                rows,
            }
        });
        let mono_ms = T0_MS + at_ms;
        let wall = self
            .at_ms
            .map_or(WALL_MS, |at| at - i64::try_from(T0_MS + w).unwrap_or(0));
        Sample {
            at_ns: mono_ms * 1_000_000,
            cores: Some(cores),
            mem: self.mem,
            thermal: self.thermal,
            procs: Some(procs),
            self_pid: self.self_pid,
            self_uid: Some(self.self_uid),
            tabs: self.tabs.clone(),
            delegated: self.delegated.clone(),
            at_unix_ms: wall + i64::try_from(mono_ms).unwrap_or(0),
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

/// Apple's identity for the process `pid` born at `birth_ms`, named `exe`, signed as `id` (the platform flag, category 1).
#[must_use]
pub fn apple(pid: u32, birth_ms: i64, exe: &str, id: &str) -> LeadIdentity {
    LeadIdentity {
        pid,
        birth_ms,
        exe: exe.to_owned(),
        voucher: Voucher::Apple,
        signing_id: Some(id.to_owned()),
        team: None,
    }
}

/// RD's machine's timeline on 2026-10-20: booted 16:06:13Z, asleep from 16:26:38Z, woken by the
/// trackpad (`RTP.multi-touch`) 5 s before userspace started.
#[must_use]
pub fn timeline_1020() -> TimelineSample {
    TimelineSample {
        boot_ms: LAUNCHD_MS - 1_000,
        sleep_ms: Some(1_792_513_598_000),
        wake_ms: Some(USERSPACE_MS - 5_000),
        wake_reason: Some("RTP.multi-touch".into()),
    }
}

/// When userspace started on 2026-10-20, 17:28:38Z (10:28:38 PDT).
pub const USERSPACE_MS: i64 = 1_792_517_318_000;
/// When launchd was born on 2026-10-20, 16:06:14Z.
pub const LAUNCHD_MS: i64 = 1_792_512_374_000;

/// The identities of the 10:32 moment's leads (RD's evidence `probes/ident-out-1.txt`,
/// launchd's `probes/why-out-1.txt`): Apple's daemons, and aterm under its
/// Developer ID.
fn identities_1032() -> Vec<LeadIdentity> {
    let start = USERSPACE_MS;
    vec![
        apple(1, LAUNCHD_MS, "launchd", "com.apple.xpc.launchd"),
        apple(357, start, "logd", "com.apple.logd"),
        apple(387, start, "mds", "com.apple.mds"),
        apple(397, start, "contextstored", "com.apple.contextstored"),
        apple(533, start + 1_000, "mds_stores", "com.apple.mds_stores"),
        apple(916, start + 7_000, "siriactionsd", "com.apple.siriactionsd"),
        LeadIdentity {
            pid: 1187,
            birth_ms: start + 197_000,
            exe: "aterm".into(),
            voucher: Voucher::DeveloperId,
            signing_id: Some("com.aterm.aterm".into()),
            team: Some("A66A9P66Z7".into()),
        },
    ]
}

/// A tab of one pane with key `key`, labelled `Tab <key>`, whose shell is `shell_pid`.
#[must_use]
pub fn tab(key: u64, shell_pid: u32) -> TabRoot {
    TabRoot {
        key,
        tab: key,
        shell_pid,
        label: format!("Tab {key}"),
        program: String::new(),
    }
}

/// RD's 10:32 moment (§2.5, Example A, constructed).
#[must_use]
pub fn spotlight_1032() -> Scene {
    let start = USERSPACE_MS;
    let launchd = LAUNCHD_MS;
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
        leads: identities_1032(),
        timeline: Some(timeline_1020()),
        lane: CoalLane::On,
        ..Scene::default()
    }
}

/// SA's 14:21 window (§7.7, one 10 s window, two tabs building).
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

/// The 10:32 moment with tab 1's live figure read past aterm's coalition (constructed).
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

/// The 10:32 moment with one coalition reaped and another created inside the window.
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

/// The 10:32 moment with tab 1's delegated job under an app server in coalition 4000.
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

/// The 10:32 moment with a tab 2 (shell pid 1250) and one app server in
/// coalition 4000 (100 millicores of its own) launching both tabs' delegated
/// jobs: tab 1's cargo (pid 4010) at 500, tab 2's (pid 4020) at 300.
#[must_use]
pub fn shared_launcher_1032() -> Scene {
    let born = 1_792_517_500_000;
    let mut scene = spotlight_1032();
    scene.tabs.push(tab(2, 1250));
    scene.procs.push(Proc::new(
        1250,
        1187,
        501,
        1342,
        USERSPACE_MS + 210_000,
        "zsh",
        0,
    ));
    scene.busy_mc += 900;
    scene.coalitions.push(Coal::new(4000, 900));
    scene
        .procs
        .push(Proc::new(4001, 1, 501, 4000, born, "app-server", 100));
    for (pid, tab, mc) in [(4010, 1, 500), (4020, 2, 300)] {
        let start = born + i64::from(pid - 4000);
        scene
            .procs
            .push(Proc::new(pid, 4001, 501, 4000, start, "cargo", mc));
        let key = crate::delegated::ProcessKey {
            pid,
            start: u64::try_from(start).unwrap_or(0),
        };
        scene.delegated.push(DelegatedJob {
            root: key,
            tab: Some(tab),
            members: vec![key],
        });
    }
    scene
}

/// The 10:32 moment with tab 1's delegated job detached to launchd inside aterm's coalition.
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

/// The 10:32 moment as tab 1's build starts.
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

/// The liar (RD §6.2): the 10:32 moment with mds and mds_stores signed ad hoc under Apple's
/// signing identifiers.
#[must_use]
pub fn liar_1032() -> Scene {
    let mut scene = spotlight_1032();
    for l in &mut scene.leads {
        if l.pid == 387 || l.pid == 533 {
            l.voucher = Voucher::AdHoc;
        }
    }
    scene
}

/// The 10:32 moment with `corespotlightd` restarted in coalition 1330 and indexing at 2.00 cores.
#[must_use]
pub fn spotlight_late_1032() -> Scene {
    let born = USERSPACE_MS + 150_000;
    let mut scene = spotlight_1032();
    scene.busy_mc += 2000;
    scene.coalitions.push(Coal {
        deferred_mc: 1200,
        ..Coal::new(1330, 2000)
    });
    scene
        .procs
        .push(Proc::new(1150, 1, 501, 1330, born, "corespotlightd", 2000));
    scene.leads.push(apple(
        1150,
        born,
        "corespotlightd",
        "com.apple.corespotlightd",
    ));
    scene
}

/// The 10:32 moment with the coalition lane off (macOS's self-check missed).
#[must_use]
pub fn lane_off_1032() -> Scene {
    Scene {
        lane: CoalLane::Off,
        ..spotlight_1032()
    }
}

/// The 10:32 moment with a build in tab 1 whose compilers exited between sweeps.
#[must_use]
pub fn finished_1032() -> Scene {
    let mut scene = spotlight_1032();
    scene.busy_mc += 3000;
    for c in &mut scene.coalitions {
        if c.id == 1342 {
            c.mc += 3000;
        }
    }
    scene
}

/// RD's calm Mac (§2.6, Example B), 15:10:02.120 PDT (22:10:02.120Z), 4 h 41 min after start-up.
#[must_use]
pub fn calm_1510() -> Scene {
    let start = USERSPACE_MS;
    let mut leads = identities_1032();
    leads.push(apple(
        430,
        start + 500,
        "WindowServer",
        "com.apple.WindowServer",
    ));
    leads.push(LeadIdentity {
        pid: 784,
        birth_ms: start + 8_000,
        exe: "Caffeine".into(),
        voucher: Voucher::DeveloperId,
        signing_id: None,
        team: Some("YD6LEYT6WZ".into()),
    });
    Scene {
        window_ms: 5000,
        sweep_ms: 1000,
        cores: 18,
        busy_mc: 400,
        self_pid: 1187,
        self_uid: 501,
        tabs: vec![tab(1, 1200)],
        coalitions: vec![
            Coal::new(1342, 200),
            Coal::new(896, 10),
            Coal::new(393, 90),
            Coal {
                deferred_mc: 15,
                ..Coal::new(307, 20)
            },
            Coal::new(1, 80),
        ],
        procs: vec![
            Proc::new(1, 0, 0, 1, LAUNCHD_MS, "launchd", 80),
            Proc::new(387, 1, 0, 307, start, "mds", 20),
            Proc::new(430, 1, 88, 393, start + 500, "WindowServer", 90),
            Proc::new(784, 1, 501, 896, start + 8_000, "Caffeine", 10),
            Proc::new(1187, 1, 501, 1342, start + 197_000, "aterm", 80),
            Proc::new(1200, 1187, 501, 1342, start + 199_000, "zsh", 0),
            Proc::new(1210, 1200, 501, 1342, start + 200_000, "claude", 120),
        ],
        leads,
        timeline: Some(timeline_1020()),
        lane: CoalLane::On,
        at_ms: Some(1_792_534_202_120),
        mem: Some(MemSample {
            total: 128 << 30,
            app: (12 << 30) + (600 << 20),
            wired: 4 << 30,
            compressed: Some(0),
            pressure: Pressure::Normal,
            counters: Some(MemCounters::default()),
            ..MemSample::default()
        }),
        thermal: Some(Thermal {
            low_power: Some(false),
            exact: true,
            ..Thermal::default()
        }),
        ..Scene::default()
    }
}

/// The calm Mac with coalition 2000 reaped inside the window after 1.50
/// cores of work (constructed): the work is in the busy figure and in no
/// row, so it lands in `unaccounted`.
#[must_use]
pub fn reaped_1510() -> Scene {
    let mut scene = calm_1510();
    scene.busy_mc += 1500;
    scene.coalitions.push(Coal::new(2000, 1500).gone());
    scene
}

/// The calm Mac with Caffeine (coalition 896, the user's app, led from
/// start-up) at 1.50 cores: app-work.
#[must_use]
pub fn app_work_1510() -> Scene {
    let mut scene = calm_1510();
    scene.busy_mc += 1490;
    for c in &mut scene.coalitions {
        if c.id == 896 {
            c.mc = 1500;
        }
    }
    for p in &mut scene.procs {
        if p.pid == 784 {
            *p = Proc::new(
                p.pid,
                p.ppid,
                p.uid,
                p.coalition,
                p.birth_ms,
                "Caffeine",
                1500,
            );
        }
    }
    scene
}

/// A tab-2 build 20 minutes after start-up (RD §6.2), 10:48:41.200 PDT.
#[must_use]
pub fn tab2_build_1048() -> Scene {
    let start = USERSPACE_MS;
    let built = start + 1_150_000;
    let mut leads = identities_1032();
    leads.push(apple(
        430,
        start + 500,
        "WindowServer",
        "com.apple.WindowServer",
    ));
    let mut procs = vec![
        Proc::new(1, 0, 0, 1, LAUNCHD_MS, "launchd", 900),
        Proc::new(357, 1, 0, 247, start, "logd", 100),
        Proc::new(387, 1, 0, 307, start, "mds", 300),
        Proc::new(397, 1, 0, 327, start, "contextstored", 200),
        Proc::new(430, 1, 88, 393, start + 500, "WindowServer", 200),
        Proc::new(1187, 1, 501, 1342, start + 197_000, "aterm", 300),
        Proc::new(1200, 1187, 501, 1342, start + 199_000, "zsh", 0),
        Proc::new(1210, 1200, 501, 1342, start + 200_000, "claude", 200),
        Proc::new(1300, 1187, 501, 1342, start + 900_000, "zsh", 0),
        Proc::new(1310, 1300, 501, 1342, built, "cargo", 200),
    ];
    procs.extend((0..4).map(|i| {
        Proc::new(
            1320 + i,
            1310,
            501,
            1342,
            built + 1_000 + i64::from(i),
            "rustc",
            1450,
        )
    }));
    Scene {
        window_ms: 5000,
        sweep_ms: 1000,
        cores: 18,
        busy_mc: 8200,
        self_pid: 1187,
        self_uid: 501,
        tabs: vec![tab(1, 1200), tab(2, 1300)],
        coalitions: vec![
            Coal::new(1342, 6500),
            Coal {
                deferred_mc: 240,
                ..Coal::new(307, 300)
            },
            Coal::new(327, 200),
            Coal::new(247, 100),
            Coal::new(393, 200),
            Coal::new(1, 900),
        ],
        procs,
        leads,
        timeline: Some(timeline_1020()),
        lane: CoalLane::On,
        at_ms: Some(1_792_518_521_200),
        ..Scene::default()
    }
}

/// The wrapped pid space (RD §6.2; RD's evidence `probes/pidwrap-out-1.txt`) at 12:18:30 PDT
/// (19:18:30Z): tab 2's shell, pid 159, is younger than every higher pid.
#[must_use]
pub fn wrapped_1218() -> Scene {
    let mut scene = spotlight_1032();
    scene.tabs.push(tab(2, 159));
    scene.procs.push(Proc::new(
        159,
        1187,
        501,
        1342,
        USERSPACE_MS + 6_449_000,
        "zsh",
        0,
    ));
    scene.at_ms = Some(1_792_523_910_000);
    scene
}
