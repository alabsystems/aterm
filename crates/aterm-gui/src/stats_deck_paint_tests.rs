// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scene and gallery tests for the stats deck heat-table painter.

use super::*;
use crate::stats_paint::HitKind;
use crate::stats_paint::tests::{job_row, zombies};
use aterm_vitals::explain::WhyFacts;
use aterm_vitals::explain::fixtures::{self, spotlight_1032};
use aterm_vitals::jobs::{Findings, JobsView, Verdict};
use aterm_vitals::model::{
    AccountIx, Busiest, CapBasis, ClusterView, CoreView, CpuView, DiskFacts, GpuSample, GpuView,
    Hog, LinkKind, LinkSample, MemSample, MemView, NoFigure, PlanFact, Pressure, ProcLimits,
    ProcScope, ScopeFig, Series, TabView, VolumeSample,
};
use aterm_vitals::{Cell, Filling, Rates, TrendView};

/// 10:42:17 on the mockups' day, unix ms.
pub(crate) const AT: i64 = 1_792_000_800_000 + 42 * 60_000 + 17_000;
pub(crate) const MIN_MS: i64 = 60_000;
pub(crate) const GIB: f64 = 1_073_741_824.0;
/// The front tab's key.
pub(crate) const FRONT: u64 = 2;
const LONG_TAB: &str = "release build of the whole workspace now";
const EPS: f64 = 0.01;

/// The scenes the spec's mockups and tests read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scene {
    /// The owner's Mac at 10:42:17: busy cores, memory and disk calm.
    Calm,
    /// Memory under pressure, the disk filling, two jobs stuck.
    Trouble,
    /// aterm a minute old: four buckets read, Network and Tokens learning.
    FirstRun,
    /// No disk watch: no level, no lines, no freed.
    HarnessOff,
    /// T-G4's worst case.
    Worst,
    /// Worst-case scene ([`Scene::Worst`]) restricted to valid CPU and memory overflow slots.
    Costly,
}

struct Rng(u64);

impl Rng {
    fn uniform(&mut self, a: f64, b: f64) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        a + (b - a) * ((self.0 >> 11) as f64 / (1_u64 << 53) as f64)
    }
}

struct Story {
    heat: [Vec<Option<f64>>; 7],
    over: [Vec<bool>; 7],
}

fn story(trouble: bool) -> Story {
    let mut r = Rng(7);
    let n = 60;
    let (mut cpu, mut cpu_over) = (Vec::new(), Vec::new());
    for j in 0..n {
        let jf = f64::from(j);
        let v = if j < 12 {
            0.47 + 0.12 * (jf * 0.9).sin() + r.uniform(-0.05, 0.05)
        } else if j < 18 {
            0.62 + (jf - 12.0) * 0.06 + r.uniform(-0.02, 0.02)
        } else if (38..=40).contains(&j) {
            0.84 + r.uniform(-0.03, 0.03)
        } else {
            0.965 + r.uniform(-0.02, 0.035)
        };
        cpu.push(v.min(1.0));
        cpu_over.push((20..=37).contains(&j) || j >= 42);
    }
    cpu[59] = 0.983;
    let mut gpu = Vec::new();
    for j in 0..n {
        let mut v = 0.09 + r.uniform(-0.04, 0.05);
        if (28..=31).contains(&j) {
            v = 0.34 + r.uniform(-0.06, 0.08);
        }
        if j == 46 || j == 47 {
            v = 0.22;
        }
        gpu.push(v.max(0.01));
    }
    gpu[59] = 0.12;
    let (mem, mem_over, disk): (Vec<f64>, Vec<bool>, Vec<f64>) = if trouble {
        let mut mem: Vec<f64> = (0..n)
            .map(|j| 0.80 + 0.08 * f64::from(j) / 59.0 + r.uniform(-0.004, 0.004))
            .collect();
        mem[59] = 112.6 / 128.0;
        let disk = (0..n)
            .map(|j| 0.955 + 0.024 * f64::from(j) / 59.0)
            .collect();
        (mem, (0..n).map(|j| j >= 44).collect(), disk)
    } else {
        let mut mem: Vec<f64> = (0..n)
            .map(|j| 0.297 + 0.032 * f64::from(j) / 59.0 + r.uniform(-0.002, 0.002))
            .collect();
        mem[59] = 42.1 / 128.0;
        let disk = (0..n)
            .map(|j| 0.5554 + 0.0013 * f64::from(j) / 59.0)
            .collect();
        (mem, vec![false; 60], disk)
    };
    let mut net = Vec::new();
    for j in 0..n {
        let mut v = 0.06 + r.uniform(-0.04, 0.06);
        if (8..=13).contains(&j) {
            v = [0.42, 0.66, 0.74, 0.71, 0.55, 0.31][(j - 8) as usize];
        }
        if (44..=46).contains(&j) {
            v = [0.24, 0.33, 0.21][(j - 44) as usize];
        }
        net.push(v.max(0.01));
    }
    net[59] = 2.1 / 11.8;
    let mut tok = Vec::new();
    for j in 0..n {
        let base = [0.08, 0.32, 0.18][((j / 7) % 3) as usize];
        let mut v = base + r.uniform(-0.06, 0.12);
        if (24..=26).contains(&j) {
            v = 0.02;
        }
        if j == 51 || j == 52 {
            v = 0.47 + r.uniform(0.0, 0.08);
        }
        tok.push(v.max(0.0));
    }
    tok[59] = 3.1 / 9.0;
    let mut procs: Vec<f64> = (0..n)
        .map(|j| 0.052 + 0.004 * f64::from(j) / 59.0)
        .collect();
    procs[59] = 897.0 / 16_000.0;
    let some = |v: Vec<f64>| v.into_iter().map(Some).collect::<Vec<_>>();
    Story {
        heat: [
            some(cpu),
            some(gpu),
            some(mem),
            some(disk),
            some(net),
            some(tok),
            some(procs),
        ],
        over: [
            cpu_over,
            vec![false; 60],
            mem_over,
            vec![false; 60],
            vec![false; 60],
            vec![false; 60],
            vec![false; 60],
        ],
    }
}

fn fig(v: f64) -> ScopeFig {
    ScopeFig {
        now: Some(v),
        ..ScopeFig::default()
    }
}

fn not_per_app() -> ScopeFig {
    ScopeFig {
        missing: Some(NoFigure::NotPerApp),
        ..ScopeFig::default()
    }
}

fn series(now: f64, capacity: f64) -> Series {
    Series {
        now: Some(now),
        capacity: Some(capacity),
        ring: Arc::from(Vec::new()),
        binds: false,
    }
}

fn row(now: f64, cap: f64, st: &Story, i: usize, aterm: ScopeFig, tab: ScopeFig) -> ResourceView {
    let (heat, over) = (&st.heat[i], &st.over[i]);
    let ring: Vec<Cell> = heat
        .iter()
        .zip(over)
        .map(|(h, &o)| {
            let v = h.map(|h| h * cap);
            Cell {
                mean: v,
                max: v,
                over: o && v.is_some(),
            }
        })
        .collect();
    ResourceView {
        now: Some(now),
        capacity: Some(cap),
        heat_now: Some((now / cap) as f32),
        over: over.last().copied().unwrap_or(false),
        ring: ring.into(),
        ring_k: AT.div_euclid(10_000),
        aterm,
        tabs: vec![(FRONT, tab)],
        ..ResourceView::default()
    }
}

const M5: [(&str, &str, &[f32]); 2] = [
    ("Super", "S", &[1.0, 0.99, 1.0, 0.98, 1.0, 0.97]),
    (
        "Performance",
        "P",
        &[
            0.99, 1.0, 0.96, 0.98, 1.0, 1.0, 0.97, 0.86, 1.0, 0.99, 0.93, 1.0,
        ],
    ),
];

fn cpu_view(clusters: &[(&str, &str, Vec<f32>)]) -> CpuView {
    let mut cores = Vec::new();
    let mut views = Vec::new();
    for (name, short, heat) in clusters {
        let first = cores.len();
        cores.extend(heat.iter().map(|&v| CoreView {
            load: TrendView::flat(v),
        }));
        views.push(ClusterView {
            name: (*name).into(),
            short: (*short).into(),
            cores: (first..cores.len()).collect(),
        });
    }
    CpuView {
        cores,
        clusters: views,
        ..CpuView::default()
    }
}

fn jobs(counts: &[(Verdict, usize)], zombie_count: u32) -> JobsView {
    let mut rows = Vec::new();
    for &(verdict, n) in counts {
        for i in 0..n {
            rows.push(job_row(&format!("{verdict:?}{i}"), verdict, None, None));
        }
    }
    JobsView {
        rows,
        findings: Findings {
            zombies: if zombie_count > 0 {
                vec![zombies(9, zombie_count, Some(FRONT))]
            } else {
                Vec::new()
            },
            ..Findings::default()
        },
        ..JobsView::default()
    }
}

/// `sc` as one snapshot at [`AT`].
pub(crate) fn scene(sc: Scene) -> Snapshot {
    let worst = matches!(sc, Scene::Worst | Scene::Costly);
    let trouble = sc == Scene::Trouble || worst;
    let mut st = story(trouble);
    match sc {
        Scene::FirstRun => {
            for (h, o) in st.heat.iter_mut().zip(st.over.iter_mut()) {
                h.iter_mut().take(56).for_each(|h| *h = None);
                o.iter_mut().take(56).for_each(|o| *o = false);
            }
        }
        Scene::Worst | Scene::Costly => {
            for (i, (h, o)) in st.heat.iter_mut().zip(st.over.iter_mut()).enumerate() {
                let over = sc == Scene::Worst || i == 0 || i == 2;
                o.fill(over);
                h[30] = None;
            }
        }
        Scene::Calm | Scene::Trouble | Scene::HarnessOff => {}
    }
    let tab_label = if worst { LONG_TAB } else { "Tab 2" };
    let mut s = Snapshot {
        seq: 360,
        at_unix_ms: AT,
        tabs: vec![
            TabView {
                key: FRONT,
                label: tab_label.into(),
                program: "claude".into(),
                ..TabView::default()
            },
            TabView {
                key: 3,
                label: "Tab 3".into(),
                program: "zsh".into(),
                ..TabView::default()
            },
        ],
        ..Snapshot::default()
    };
    let f = &mut s.facts;

    let clusters: Vec<(&str, &str, Vec<f32>)> = if worst {
        vec![("CPU", "", vec![0.97; 128])]
    } else {
        M5.iter().map(|(n, s, h)| (*n, *s, h.to_vec())).collect()
    };
    let cpu = cpu_view(&clusters);
    let n = cpu.cores.len() as f64;
    let (now, aterm, tab) = if worst {
        (127.9, 127.9, 99.9)
    } else {
        (17.7, 6.8, 0.1)
    };
    s.resources[0] = row(now, n, &st, 0, fig(aterm), fig(tab));
    s.cpu = Some(cpu);
    f.load1 = Some(if worst { 999.9 } else { 25.7 });
    f.busiest = Some(if worst {
        Busiest {
            name: "cc1plus-with-a-long-name".into(),
            cores: 99.9,
            n: 999,
            scope: Some(ProcScope::Tab(FRONT)),
        }
    } else {
        Busiest {
            name: "rustc".into(),
            cores: 5.9,
            n: 9,
            scope: Some(ProcScope::Tab(3)),
        }
    });

    let mut gpu = row(12.0, 100.0, &st, 1, not_per_app(), not_per_app());
    if worst {
        gpu.state = Some(State::Warn);
        f.gpu_resets = Some((99, AT - 180 * MIN_MS));
    }
    s.resources[1] = gpu;
    s.gpu = Some(GpuView {
        load: TrendView::flat(0.12),
        cores: Some(40),
        ..GpuView::default()
    });
    f.gpu = Some(GpuSample {
        device_pct: Some(12),
        cores: Some(if worst { 999 } else { 40 }),
        in_use_bytes: Some(((if worst { 999.9 } else { 1.7 }) * GIB) as u64),
        ..GpuSample::default()
    });

    let (now, cap, aterm, tab, cached, swap, pressure) = match sc {
        Scene::Worst | Scene::Costly => (
            1023.9,
            1024.0,
            1023.9,
            999.9,
            999.9,
            999.9,
            Pressure::Critical,
        ),
        Scene::Trouble => (112.6, 128.0, 61.0, 0.2, 2.1, 6.4, Pressure::Warn),
        _ => (42.1, 128.0, 5.0, 0.18, 38.5, 0.0, Pressure::Normal),
    };
    let mut mem = row(
        now * GIB,
        cap * GIB,
        &st,
        2,
        fig(aterm * GIB),
        fig(tab * GIB),
    );
    mem.state = match pressure {
        Pressure::Warn => Some(State::Warn),
        Pressure::Critical => Some(State::Critical),
        _ => None,
    };
    s.resources[2] = mem;
    s.mem = Some(MemView {
        sample: MemSample {
            total: (cap * GIB) as u64,
            app: ((now - 8.0) * GIB) as u64,
            wired: (4.0 * GIB) as u64,
            compressed: Some((4.0 * GIB) as u64),
            cached: (cached * GIB) as u64,
            free: ((cap - now - cached).max(0.0) * GIB) as u64,
            swap_used: (swap * GIB) as u64,
            swap_total: (swap * GIB) as u64,
            pressure,
            commit: None,
            ..MemSample::default()
        },
        ..MemView::default()
    });
    f.swap_rate = match sc {
        Scene::Worst | Scene::Costly => Some(9_999.0 * 1_048_576.0),
        Scene::Trouble => Some(36.0 * 1_048_576.0),
        _ => None,
    };

    let total = 2_000_000_000_000_u64;
    let (avail, aterm, tab) = match sc {
        Scene::Worst | Scene::Costly => (40_000_000_000_u64, 9_999e6, 9_999e6),
        Scene::Trouble => (41_000_000_000, 410e6, 0.2e6),
        _ => (890_000_000_000, 38e6, 0.2e6),
    };
    let used = (total - avail) as f64;
    let mut disk = row(used, total as f64, &st, 3, fig(aterm), fig(tab));
    s.volume = Some(VolumeSample {
        total,
        free: avail,
        available: avail,
        mount: "/".into(),
    });
    let watch = DiskFacts {
        level: Some(DiskLevel::Ok),
        warn: Some(69_000_000_000),
        floor: Some(34_000_000_000),
        emergency: Some(8_600_000_000),
        watch: true,
        freed_today: Some((41_000_000_000, AT - 90 * MIN_MS, "build caches".into())),
        hogs: vec![Hog {
            path: "~/aterm/target".into(),
            bytes: 212_000_000_000,
            partial: false,
            remedy: String::new(),
        }],
        ..DiskFacts::default()
    };
    f.disk_rates = Rates {
        rate10: Some(2.6e9 / 600.0),
        ..Rates::default()
    };
    match sc {
        Scene::Calm => f.disk = Some(watch),
        Scene::HarnessOff => f.disk = None,
        Scene::FirstRun => {
            f.disk = Some(DiskFacts {
                freed_today: None,
                hogs: Vec::new(),
                ..watch
            });
            f.disk_rates = Rates::default();
        }
        Scene::Trouble => {
            disk.state = Some(State::Warn);
            f.filling = Some(Filling {
                secs: 34.0 * 60.0,
                rate: 20e6,
                burst: true,
            });
            f.disk_rates.rate10 = Some(12e9 / 600.0);
            f.disk = Some(DiskFacts {
                level: Some(DiskLevel::Low),
                freed_today: Some((12_000_000_000, AT - 90 * MIN_MS, "build caches".into())),
                ..watch
            });
        }
        Scene::Worst | Scene::Costly => {
            disk.state = Some(State::Critical);
            f.filling = Some(Filling {
                secs: 59.0 * 60.0,
                rate: 999e9 / 60.0,
                burst: true,
            });
            f.disk_rates.rate10 = Some(999e9 / 600.0);
            f.disk = Some(DiskFacts {
                level: Some(DiskLevel::EmergencyFiles),
                stuck: 999,
                others: vec![("Data".into(), DiskLevel::Short, 21_000_000_000)],
                stalled_since: Some(AT - 180 * MIN_MS),
                freed_today: Some((999_000_000_000, AT - 90 * MIN_MS, "build caches".into())),
                hogs: vec![Hog {
                    path: "~/src/a/very/long/path/to/its/build/target".into(),
                    bytes: 999_000_000_000,
                    partial: true,
                    remedy: String::new(),
                }],
                ..watch
            });
        }
    }
    s.resources[3] = disk;

    let mut net = if worst {
        let mut v = row(9_999e6, 9_999e6, &st, 4, not_per_app(), not_per_app());
        v.second = Some(series(9_999e6, 9_999e6));
        v
    } else if sc == Scene::FirstRun {
        let mut v = row(2.1e6, 22.9e6, &st, 4, not_per_app(), not_per_app());
        v.second = Some(series(0.3e6, 22.9e6));
        v
    } else {
        let mut v = row(2.1e6, 11.8e6, &st, 4, not_per_app(), not_per_app());
        v.second = Some(series(0.3e6, 4.2e6));
        v
    };
    net.basis = if sc == Scene::FirstRun {
        CapBasis::Link
    } else {
        CapBasis::Peak
    };
    s.resources[4] = net;
    f.link = Some(LinkSample {
        name: if worst { "en10" } else { "en0" }.into(),
        kind: LinkKind::Wifi,
        up: true,
        counts: true,
        rx: 0,
        tx: 0,
        rate_bps: Some(if worst { 99_999_000_000 } else { 183_200_000 }),
        ..LinkSample::default()
    });
    f.net_read_s = if sc == Scene::FirstRun { 0 } else { 86_400 };

    let mut tokens = if worst {
        row(99_900.0, 99_900.0, &st, 5, fig(99_900.0), fig(99_900.0))
    } else {
        row(3_100.0, 9_000.0, &st, 5, fig(3_100.0), fig(1_200.0))
    };
    tokens.basis = CapBasis::Peak;
    if sc == Scene::FirstRun {
        tokens.basis = CapBasis::Learning;
        tokens.capacity = None;
        tokens.heat_now = None;
    }
    s.resources[5] = tokens;
    f.tokens_in = Some(if worst { 99.9e6 } else { 410_000.0 });
    f.agents = 1;
    f.tokens_read_s = if sc == Scene::FirstRun { 60 } else { 3_600 };
    if worst {
        f.plan = Some(PlanFact {
            window_word: "weekly".into(),
            used_x10: 999,
            read_at: AT - 4 * MIN_MS,
            account: AccountIx(0),
        });
    }

    let mut procs = if worst {
        let mut v = row(
            999_999.0,
            9_999_999.0,
            &st,
            6,
            fig(999_999.0),
            fig(999_999.0),
        );
        v.second = Some(series(999_999.0, 4_194_304.0));
        v
    } else {
        let mut v = row(897.0, 16_000.0, &st, 6, fig(42.0), fig(3.0));
        v.second = Some(series(578.0, 10_666.0));
        v
    };
    procs.heat_now = procs.now.zip(procs.capacity).map(|(n, c)| (n / c) as f32);
    s.resources[6] = procs;
    f.pid_counts = if worst {
        (Some(999_999), Some(999_999))
    } else {
        (Some(897), Some(578))
    };
    f.proc_limits = ProcLimits {
        system: Some(if worst { 9_999_999 } else { 16_000 }),
        per_user: Some(if worst { 4_194_304 } else { 10_666 }),
    };
    let view = match sc {
        Scene::Worst | Scene::Costly => jobs(
            &[
                (Verdict::Stuck, 999),
                (Verdict::Quiet, 999),
                (Verdict::WaitingYou, 999),
                (Verdict::Parked, 999),
            ],
            999,
        ),
        Scene::Trouble => jobs(&[(Verdict::Stuck, 2), (Verdict::Quiet, 1)], 0),
        _ => jobs(&[(Verdict::Stuck, 1), (Verdict::Quiet, 2)], 0),
    };
    s.set_jobs(Some(Arc::new(view)));
    if trouble {
        s.why = Some(spotlight_after_start_up());
    }
    s
}

/// The cause engine's facts on RD's 10:32 scene (`explain::fixtures`) after
/// four of its windows, so the Deck's clause holds: Spotlight, after start-up
/// (RD §4.1), the troubled scenes' CPU cause, with the open episode and the
/// split it stands on, its one tab the scenes' front tab. Read once: a sweep
/// draws thousands of scenes.
pub(crate) fn spotlight_after_start_up() -> Arc<WhyFacts> {
    static FACTS: std::sync::OnceLock<Arc<WhyFacts>> = std::sync::OnceLock::new();
    Arc::clone(FACTS.get_or_init(|| {
        let mut vitals = aterm_vitals::Vitals::new();
        let scene = fixtures::Scene {
            tabs: vec![fixtures::tab(FRONT, 1200)],
            ..spotlight_1032()
        };
        let mut snaps: Vec<Snapshot> = scene.series(4).iter().map(|r| vitals.absorb(r)).collect();
        snaps.pop().and_then(|s| s.why).expect("the scene's facts")
    }))
}

const MAC: Locale = Locale {
    platform: Platform::MacOs,
    utc_offset_s: 0,
};

pub(crate) fn light_inks() -> VitalsInks {
    ink::vitals_inks([0xE2, 0xE3, 0xE5], [0x24, 0x29, 0x2F], [0x1A, 0x7F, 0x37])
}

pub(crate) fn scene_input(snap: &Snapshot, form: DeckForm) -> DeckInput<'_> {
    let long = snap.tab(FRONT).is_some_and(|t| t.label == LONG_TAB);
    DeckInput {
        snap: Some(snap),
        tab_key: Some(FRONT),
        tab_panes: vec![FRONT],
        tab_label: if long { LONG_TAB } else { "Tab 2" }.into(),
        tab_count: 2,
        inks: super::tests::dark_inks(),
        dwc: false,
        marks: Vec::new(),
        plate: None,
        form,
        contrast: false,
        locale: MAC,
    }
}

fn deck_pt(form: DeckForm, s: f64) -> f64 {
    deck_px(StatsBars::Deck, form, s, (14.0 * s) as usize) as f64 / s
}

fn geom(w: f64, s: f64, form: DeckForm) -> DeckGeom {
    DeckGeom {
        w: (w * s) as f32,
        h: (deck_pt(form, s) * s) as f32,
        pad: (12.0 * s) as f32,
        scale: s as f32,
        font_px: (13.0 * s) as f32,
    }
}

fn laid(sc: Scene, w: f64, s: f64, form: DeckForm, m: &FaceMetrics, draw: bool) -> Laid {
    let snap = scene(sc);
    let input = scene_input(&snap, form);
    let words = DeckWords::new(&input, m);
    lay(&geom(w, s, form), &input, &words, m, draw)
}

fn sweep_widths() -> impl Iterator<Item = f64> {
    (160..=5120).map(f64::from)
}

struct Swept<'a> {
    sc: Scene,
    face: &'static str,
    m: &'a FaceMetrics,
    s: f64,
    form: DeckForm,
    w: f64,
    laid: Laid,
}

/// What a check of the T-G4 sweep reads in a layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Check {
    /// T-G4: no text overlaps or leaves the card.
    Whole,
    /// T-G14: the key's "over max" and time label.
    Key,
    /// T-G9: the cells a width draws.
    Cells,
}

/// What each check found over the T-G4 sweep: one pass lays every scene out at every width, and
/// each test reads its own check's findings (the key and the cells on the worst scene only).
fn swept(check: Check) -> Vec<&'static str> {
    static FOUND: std::sync::OnceLock<Vec<(Check, String)>> = std::sync::OnceLock::new();
    let scenes = [
        Scene::Worst,
        Scene::Calm,
        Scene::Trouble,
        Scene::FirstRun,
        Scene::HarnessOff,
    ];
    FOUND
        .get_or_init(|| {
            sweep(&scenes, |d| {
                let worst = d.sc == Scene::Worst;
                let tagged = |check, found: Vec<String>| found.into_iter().map(move |w| (check, w));
                tagged(Check::Whole, whole(d))
                    .chain(tagged(Check::Key, if worst { key(d) } else { Vec::new() }))
                    .chain(tagged(
                        Check::Cells,
                        if worst { cells(d) } else { Vec::new() },
                    ))
                    .collect()
            })
        })
        .iter()
        .filter(|(c, _)| *c == check)
        .map(|(_, why)| why.as_str())
        .collect()
}

fn sweep(
    scenes: &[Scene],
    check: impl Fn(&Swept<'_>) -> Vec<(Check, String)> + Sync,
) -> Vec<(Check, String)> {
    let snaps: Vec<(Scene, Snapshot)> = scenes.iter().map(|&sc| (sc, scene(sc))).collect();
    let faces = [
        ("Helvetica Neue", FaceMetrics::helvetica_neue()),
        ("the test stack", FaceMetrics::test_stack()),
    ];
    let mut jobs = Vec::new();
    for (sc, snap) in &snaps {
        for (face, m) in &faces {
            for s in [1.0, 2.0] {
                for form in [DeckForm::Full, DeckForm::Folded] {
                    jobs.push((*sc, snap, *face, m, s, form));
                }
            }
        }
    }
    let check = &check;
    std::thread::scope(|scope| {
        let running: Vec<_> = jobs
            .into_iter()
            .map(|(sc, snap, face, m, s, form)| {
                scope.spawn(move || {
                    let input = scene_input(snap, form);
                    let words = DeckWords::new(&input, m);
                    let mut failed: Vec<(Check, String)> = Vec::new();
                    for w in sweep_widths() {
                        let laid = lay(&geom(w, s, form), &input, &words, m, false);
                        let swept = Swept {
                            sc,
                            face,
                            m,
                            s,
                            form,
                            w,
                            laid,
                        };
                        for (c, why) in check(&swept) {
                            if failed.iter().filter(|(f, _)| *f == c).count() <= 20 {
                                failed.push((
                                    c,
                                    format!("{sc:?}, {face}, {s}×, {form:?}, {w} pt: {why}"),
                                ));
                            }
                        }
                    }
                    failed
                })
            })
            .collect();
        running
            .into_iter()
            .flat_map(|run| {
                run.join().unwrap_or_else(|_| {
                    [Check::Whole, Check::Key, Check::Cells]
                        .map(|c| (c, "a sweep panicked".to_string()))
                        .into()
                })
            })
            .collect()
    })
}

fn overlap(a: &[f64; 4], b: &[f64; 4]) -> bool {
    a[0] < b[2] - EPS && b[0] < a[2] - EPS && a[1] < b[3] - EPS && b[1] < a[3] - EPS
}

fn whole(d: &Swept<'_>) -> Vec<String> {
    let (w, h) = (d.w, deck_pt(d.form, d.s));
    let mut why = Vec::new();
    let boxes = &d.laid.boxes;
    let names = DECK_RESOURCES
        .iter()
        .filter(|&&r| boxes.iter().any(|b| b.text == words::resource_name(r)))
        .count();
    if d.laid.head.is_some() && names < DECK_RESOURCES.len() {
        why.push(format!("CONTROL: {names} rows named"));
    }
    for b in boxes {
        if b.r[0] < -EPS || b.r[1] < -EPS || b.r[2] > w + EPS || b.r[3] > h + EPS {
            why.push(format!(
                "{:?} {:?} {:?} leaves the card",
                b.kind, b.text, b.r
            ));
        }
        if let Some((x0, x1)) = b.room
            && (b.r[0] < x0 - EPS || b.r[2] > x1 + EPS)
        {
            why.push(format!(
                "{:?} {:?} leaves its column {x0}..{x1}",
                b.text, b.r
            ));
        }
    }
    let mut by_x: Vec<&Boxed> = boxes.iter().collect();
    by_x.sort_by(|a, b| a.r[0].total_cmp(&b.r[0]));
    for (i, a) in by_x.iter().enumerate() {
        for b in by_x.iter().skip(i + 1) {
            if b.r[0] >= a.r[2] - EPS {
                break;
            }
            let tick = matches!(
                (a.kind, b.kind),
                (BoxKind::Bar, BoxKind::Tick) | (BoxKind::Tick, BoxKind::Bar)
            );
            if !tick && overlap(&a.r, &b.r) {
                why.push(format!(
                    "{:?} {:?} {:?} meets {:?} {:?} {:?}",
                    a.kind, a.text, a.r, b.kind, b.text, b.r
                ));
            }
        }
    }
    why
}

#[test]
fn no_text_overlaps_and_none_leaves_the_card() {
    let failed = swept(Check::Whole);
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

fn key(d: &Swept<'_>) -> Vec<String> {
    let mut why = Vec::new();
    let Some(head) = d.laid.head.as_ref() else {
        return why;
    };
    let over_w = KeyItem::Over.width(d.m);
    let fits = head.key.spans.iter().any(|s| s.x1 - s.x0 >= over_w - EPS);
    let drawn = |text: &str| {
        d.laid
            .boxes
            .iter()
            .filter(|b| b.kind == BoxKind::Text && b.text == text)
            .count()
    };
    if fits && drawn("over max") == 0 {
        why.push("a span fits \"over max\" and it is not drawn".into());
    }
    let both_states = head
        .labels
        .iter()
        .filter(|l| l.label == Label::State)
        .count()
        == 2;
    if !fits && !both_states {
        why.push(format!("no span fits \"over max\": {:?}", head.key.spans));
    }
    if d.form == DeckForm::Folded && drawn("10 min ago") != 1 {
        why.push(format!("the time label {} times", drawn("10 min ago")));
    }
    let _ = (d.face, d.s);
    why
}

#[test]
fn key_is_placed() {
    let failed = swept(Check::Key);
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

fn cells(d: &Swept<'_>) -> Vec<String> {
    if d.laid.cells > 420 {
        vec![format!("{} cells", d.laid.cells)]
    } else {
        Vec::new()
    }
}

#[test]
fn work_counts_deck() {
    let m = FaceMetrics::helvetica_neue();
    for form in [DeckForm::Full, DeckForm::Folded] {
        let laid = laid(Scene::Costly, 2560.0, 2.0, form, &m, true);
        assert!(
            laid.prims.len() <= 1000,
            "{form:?}: {} prims",
            laid.prims.len()
        );
        assert!(laid.prims.len() > 400, "CONTROL: {form:?} drew the table");
    }
    let failed = swept(Check::Cells);
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

fn cpu_details(w: f64, form: DeckForm, m: &FaceMetrics) -> Option<(Col, f64, f64)> {
    match form {
        DeckForm::Full => {
            let p = plan::plan_full(w, m);
            p.details.map(|d| (d, p.rows.top(0), p.rows.cell))
        }
        DeckForm::Folded => {
            let b = plan::plan_folded(w, m).blocks[0];
            b.details.map(|d| (d, b.rows.top(0), b.rows.cell))
        }
    }
}

#[test]
fn the_cause_clause_folds_before_load_and_after_busiest() {
    let m = FaceMetrics::helvetica_neue();
    let sep = " \u{b7} ";
    let drawn = |sc: Scene, w: f64, form: DeckForm| -> Option<(Vec<String>, Vec<Boxed>)> {
        let laid = laid(sc, w, 2.0, form, &m, false);
        let (col, top, cell) = cpu_details(w, form, &m)?;
        let texts = texts_in(&laid, col, top - 1.0, top + cell + 1.0);
        let marks = laid
            .boxes
            .iter()
            .filter(|b| {
                b.kind == BoxKind::Mark && b.r[0] >= col.x - EPS && b.r[2] <= col.end() + EPS
            })
            .filter(|b| {
                (b.r[1] + b.r[3]) / 2.0 > top - 1.0 && (b.r[1] + b.r[3]) / 2.0 < top + cell + 1.0
            })
            .cloned()
            .collect();
        Some((texts, marks))
    };
    let (wide, marks) = drawn(Scene::Trouble, 1424.0, DeckForm::Full).expect("Details at W 1424");
    assert_eq!(
        wide,
        [
            "8 waiting for a core (load 25.7)",
            sep,
            "Spotlight, after start-up",
            sep,
            "busiest rustc \u{d7}9 in Tab 3, 5.9 cores",
        ]
    );
    let cause: Vec<&Boxed> = marks.iter().filter(|b| b.text == "cause").collect();
    assert_eq!(cause.len(), 1, "one \u{2235}: {marks:?}");
    let full = laid(Scene::Trouble, 1424.0, 2.0, DeckForm::Full, &m, true);
    let words = full
        .boxes
        .iter()
        .find(|b| b.kind == BoxKind::Text && b.text == "Spotlight, after start-up")
        .expect("the clause's words");
    assert!(
        cause[0].r[2] <= words.r[0] + EPS && words.r[0] - cause[0].r[2] < 0.7 * SMALL_PT,
        "the mark leads its words: {:?} then {:?}",
        cause[0].r,
        words.r
    );
    let inks = super::tests::dark_inks();
    assert_ne!(inks.words2, inks.words, "CONTROL: the inks differ");
    let [x0, y0, x1, y1] = cause[0].r.map(|v| (v * 2.0) as f32);
    let mut dots: Vec<(f32, f32, [u8; 4])> = full
        .prims
        .iter()
        .filter_map(|p| match *p {
            DrawPrim::Dot { cx, cy, color, .. }
                if (x0..=x1).contains(&cx) && (y0..=y1).contains(&cy) =>
            {
                Some((cx, cy, color))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        dots.iter().map(|d| d.2).collect::<Vec<_>>(),
        [rgba(inks.words2, 0xFF); 3],
        "the \u{2235}'s dots"
    );
    dots.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.total_cmp(&b.0)));
    let [(lx, ly, _), (rx, ry, _), (bx, by, _)] = dots[..] else {
        panic!("three dots: {dots:?}")
    };
    assert!(
        ly == ry && ry < by && lx < bx && bx < rx,
        "\u{2235}, not \u{2234}: {dots:?}"
    );

    let (mid, _) = drawn(Scene::Trouble, 1119.0, DeckForm::Full).expect("Details at W 1119");
    assert_eq!(
        mid,
        ["8 waiting for a core (load 25.7)", sep, "Spotlight"],
        "busiest folds first, then the cause shortens"
    );
    let (col, ..) = cpu_details(1175.0, DeckForm::Full, &m).expect("Details at W 1175");
    assert!((col.w - 250.0).abs() < 0.5, "a 250 pt column: {col:?}");
    let (between, _) = drawn(Scene::Trouble, 1175.0, DeckForm::Full).expect("Details");
    assert_eq!(
        between,
        [
            "load 25.7 on 18 cores",
            sep,
            "Spotlight",
            sep,
            "rustc \u{d7}9 in Tab 3"
        ],
        "all three short"
    );
    assert_eq!(
        drawn(Scene::Trouble, 900.0, DeckForm::Full),
        None,
        "no Details at W 900"
    );
    let narrow = laid(Scene::Trouble, 900.0, 2.0, DeckForm::Full, &m, false);
    assert!(
        narrow
            .boxes
            .iter()
            .all(|b| b.text != "cause" && !b.text.starts_with("Spotlight")),
        "no clause where no Details column shows"
    );

    let mut seen = 0;
    for form in [DeckForm::Full, DeckForm::Folded] {
        for w in (160..=5120).step_by(7).map(f64::from) {
            if let Some((calm, marks)) = drawn(Scene::Calm, w, form) {
                assert!(
                    calm.iter().all(|t| !t.starts_with("Spotlight"))
                        && marks.iter().all(|b| b.text != "cause"),
                    "Calm holds no cause, {form:?} {w} pt: {calm:?}"
                );
            }
            let Some((texts, marks)) = drawn(Scene::Trouble, w, form) else {
                continue;
            };
            let at = |p: &dyn Fn(&str) -> bool| texts.iter().position(|t| p(t.as_str()));
            let load = at(&|t| t.starts_with("load") || t.starts_with("8 waiting"));
            let cause = at(&|t| t.starts_with("Spotlight"));
            let busiest = at(&|t| t.starts_with("busiest") || t.starts_with("rustc"));
            let order = match (load, cause, busiest) {
                (_, None, Some(_)) => false,
                (None, Some(_), _) => false,
                (Some(l), Some(c), b) => l < c && b.is_none_or(|b| c < b),
                _ => true,
            };
            assert!(order, "{form:?} {w} pt: {texts:?}");
            assert_eq!(
                marks.iter().filter(|b| b.text == "cause").count(),
                usize::from(cause.is_some()),
                "{form:?} {w} pt: {texts:?}"
            );
            seen += usize::from(cause.is_some());
        }
    }
    assert!(seen > 100, "CONTROL: the clause drawn at {seen} widths");
}

fn frame(laid: &Laid) -> Vec<(BoxKind, [f64; 4])> {
    laid.boxes
        .iter()
        .filter(|b| matches!(b.kind, BoxKind::Strip | BoxKind::Bar | BoxKind::Core))
        .map(|b| (b.kind, b.r))
        .collect()
}

#[test]
fn layout_ignores_data() {
    let m = FaceMetrics::helvetica_neue();
    for form in [DeckForm::Full, DeckForm::Folded] {
        for s in [1.0, 2.0] {
            for w in [
                200.0, 323.0, 350.0, 450.0, 522.0, 550.0, 839.0, 1045.0, 1119.0, 2560.0,
            ] {
                let calm = laid(Scene::Calm, w, s, form, &m, true);
                let trouble = laid(Scene::Trouble, w, s, form, &m, true);
                assert_eq!(frame(&calm), frame(&trouble), "{form:?} {w} pt at {s}×");
                assert!(!frame(&calm).is_empty() || w < 300.0, "CONTROL: {w}");
            }
        }
    }
}

#[test]
fn core_squares_never_touch() {
    let m = FaceMetrics::helvetica_neue();
    let mut squares = 0;
    for sc in [Scene::Calm, Scene::Worst] {
        for form in [DeckForm::Full, DeckForm::Folded] {
            for s in [1.0, 2.0] {
                for w in (300..=2600).step_by(13).map(f64::from) {
                    let laid = laid(sc, w, s, form, &m, true);
                    for b in laid.boxes.iter().filter(|b| b.kind == BoxKind::Core) {
                        let [x0, y0, x1, y1] = b.r.map(|v| px(v, s));
                        let inside: Vec<(f32, f32, f32, f32)> = laid
                            .prims
                            .iter()
                            .filter_map(|p| match *p {
                                DrawPrim::Panel { x, y, w, h, .. }
                                    if x >= x0 - 1.0
                                        && y >= y0 - 1.0
                                        && x + w <= x1 + 1.0
                                        && y + h <= y1 + 1.0 =>
                                {
                                    Some((x, y, w, h))
                                }
                                _ => None,
                            })
                            .collect();
                        squares += inside.len();
                        for (i, a) in inside.iter().enumerate() {
                            for c in &inside[i + 1..] {
                                let gap_x = (c.0 - (a.0 + a.2)).max(a.0 - (c.0 + c.2));
                                let gap_y = (c.1 - (a.1 + a.3)).max(a.1 - (c.1 + c.3));
                                assert!(
                                    gap_x >= 1.0 || gap_y >= 1.0,
                                    "{sc:?} {form:?} {w} pt at {s}×: {a:?} touches {c:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(squares > 10_000, "CONTROL: {squares} squares drawn");
}

#[test]
fn freed_reaches_the_compact_amounts() {
    let m = FaceMetrics::helvetica_neue();
    for (w, form) in [
        (1119.0, DeckForm::Folded),
        (350.0, DeckForm::Full),
        (348.0, DeckForm::Full),
    ] {
        for s in [1.0, 2.0] {
            let laid = laid(Scene::Calm, w, s, form, &m, false);
            let amount = laid
                .boxes
                .iter()
                .find(|b| b.kind == BoxKind::Text && b.text.contains("TB used"))
                .map(|b| b.text.as_str());
            assert_eq!(
                amount,
                Some("1.11 TB used · freed 41 GB"),
                "{form:?} {w} pt at {s}×"
            );
        }
    }
}

fn px(v: f64, s: f64) -> f32 {
    (v * s + 0.5).floor() as f32
}

type StripRows = (Strip, Rows, Vec<(u32, Resource)>);

fn strips(w: f64, form: DeckForm, m: &FaceMetrics) -> Vec<StripRows> {
    match form {
        DeckForm::Full => {
            let p = plan::plan_full(w, m);
            let rows = DECK_RESOURCES
                .iter()
                .copied()
                .zip(0_u32..)
                .map(|(r, i)| (i, r));
            vec![(p.strip, p.rows, rows.collect())]
        }
        DeckForm::Folded => {
            let p = plan::plan_folded(w, m);
            p.blocks
                .iter()
                .enumerate()
                .map(|(bi, b)| {
                    let rows = DECK_RESOURCES
                        .iter()
                        .copied()
                        .skip(bi * 4)
                        .take(4)
                        .zip(0_u32..)
                        .map(|(r, i)| (i, r));
                    (b.strip, b.rows, rows.collect())
                })
                .collect()
        }
    }
}

#[test]
fn a_gap_is_never_drawn_as_step_zero() {
    let m = FaceMetrics::helvetica_neue();
    let snap = scene(Scene::FirstRun);
    let inks = super::tests::dark_inks();
    let heat: Vec<[u8; 4]> = inks
        .heat
        .iter()
        .chain(&inks.heat_ic)
        .map(|&c| rgba(c, 0xFF))
        .collect();
    let hollow = rgba(inks.hollow, 0xFF);
    let (mut outlines, mut ticks) = (0, 0);
    for (form, w, s) in [
        (DeckForm::Full, 1119.0, 2.0),
        (DeckForm::Full, 1119.0, 1.0),
        (DeckForm::Full, 350.0, 1.0),
        (DeckForm::Folded, 1119.0, 2.0),
        (DeckForm::Folded, 450.0, 1.0),
        (DeckForm::Folded, 350.0, 1.0),
    ] {
        let input = scene_input(&snap, form);
        let words = DeckWords::new(&input, &m);
        let laid = lay(&geom(w, s, form), &input, &words, &m, true);
        for (strip, rows, at) in strips(w, form, &m) {
            let (pitch, cw) = (px(strip.pitch, s), px(strip.cell, s));
            for (r, res) in at {
                if words::strip_words(res, &snap).is_some() {
                    continue;
                }
                let n = strip.cols as usize;
                let cols = snap.resources[res.index()].heat_columns(strip.period_s, n, AT);
                let (y, h) = (px(rows.top(r), s), px(rows.cell, s));
                for (i, col) in cols.iter().enumerate() {
                    if col.heat.is_some() {
                        continue;
                    }
                    let x = px(strip.x, s) + i as f32 * pitch;
                    let at = format!("{form:?} {w} pt at {s}×, {res:?} column {i}");
                    let inside = |px0: f32, py0: f32, pw: f32, ph: f32| {
                        px0 < x + cw && px0 + pw > x && py0 < y + h && py0 + ph > y
                    };
                    for p in &laid.prims {
                        if let DrawPrim::Panel {
                            x: px0,
                            y: py0,
                            w: pw,
                            h: ph,
                            fill,
                            ..
                        } = p
                        {
                            assert!(
                                !(heat.contains(fill) && inside(*px0, *py0, *pw, *ph)),
                                "{at}: a heat cell"
                            );
                        }
                    }
                    let outline = laid.prims.iter().any(|p| {
                        matches!(p, DrawPrim::Stroke { x: sx, y: sy, color, width, .. }
                            if *sx == x + 0.5 && *sy == y + 0.5 && *color == hollow && *width == 1.0)
                    });
                    let tick = laid.prims.iter().any(|p| {
                        matches!(p, DrawPrim::Panel { x: tx, y: ty, w: tw, h: th, fill, .. }
                            if *tx == x && *ty == y + h - 1.0 && *tw == cw && *th == 1.0
                                && *fill == hollow)
                    });
                    let small = cw < 4.0 || h < 4.0;
                    assert!(if small { tick } else { outline }, "{at}: {cw}×{h} px");
                    outlines += usize::from(outline);
                    ticks += usize::from(tick);
                }
            }
        }
    }
    assert!(
        outlines > 0 && ticks > 0,
        "CONTROL: both forms drawn ({outlines}, {ticks})"
    );
}

fn full_width(kind: FullKind, m: &FaceMetrics) -> f64 {
    let ws: Vec<f64> = sweep_widths()
        .filter(|&w| {
            let p = plan::plan_full(w, m);
            !p.ground_only && p.kind == kind
        })
        .collect();
    ws.get(ws.len() / 3).copied().unwrap_or(0.0)
}

fn folded_width(kind: BlockKind, m: &FaceMetrics) -> f64 {
    let ws: Vec<f64> = sweep_widths()
        .filter(|&w| {
            let p = plan::plan_folded(w, m);
            !p.ground_only && p.blocks.iter().all(|b| b.kind == kind)
        })
        .collect();
    ws.get(ws.len() / 3).copied().unwrap_or(0.0)
}

fn texts_in(laid: &Laid, col: Col, y0: f64, y1: f64) -> Vec<String> {
    let mut found: Vec<&Boxed> = laid
        .boxes
        .iter()
        .filter(|b| {
            b.kind == BoxKind::Text
                && b.room == Some((col.x, col.end()))
                && (b.r[1] + b.r[3]) / 2.0 > y0
                && (b.r[1] + b.r[3]) / 2.0 < y1
        })
        .collect();
    found.sort_by(|a, b| a.r[0].total_cmp(&b.r[0]));
    found.iter().map(|b| b.text.clone()).collect()
}

/// Rule S on one row: with no Details, its amount slot says its state, at the first tier that fits.
#[expect(
    clippy::too_many_arguments,
    reason = "one row's words, column and band"
)]
fn rule_s(
    laid: &Laid,
    m: &FaceMetrics,
    snap: &Snapshot,
    res: Resource,
    col: Col,
    rows: Rows,
    r: u32,
    at: &str,
) {
    let Some(st) = words::state(res, snap, &MAC) else {
        return;
    };
    let pt = rows.amount_pt;
    let top = rows.top(r);
    let drawn = texts_in(laid, col, top - 1.0, top + rows.cell + 1.0);
    let mut says: Vec<&str> = st.clauses.iter().map(String::as_str).collect();
    says.push(&st.word);
    let pair = says.iter().find_map(|said| {
        st.amount_forms
            .iter()
            .find(|am| {
                m.width(am, pt, false) + 6.0 + 1.1 * pt + m.width(said, pt, true) <= col.w + EPS
            })
            .map(|am| vec![am.clone(), (*said).to_owned()])
    });
    match pair {
        Some(pair) => assert_eq!(drawn, pair, "{at}: {res:?}"),
        None => {
            let alone = says
                .iter()
                .find(|said| 1.1 * pt + m.width(said, pt, true) <= col.w + EPS);
            match alone {
                Some(said) => assert_eq!(drawn, vec![(*said).to_owned()], "{at}: {res:?}"),
                None => {
                    let cut = drawn
                        .first()
                        .map(|s| s.trim_end_matches('\u{2026}').to_owned());
                    assert!(
                        drawn.len() == 1 && cut.is_some_and(|c| st.word.starts_with(&c)),
                        "{at}: {res:?} says {drawn:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn state_is_said_in_words_without_details() {
    let m = FaceMetrics::helvetica_neue();
    let snap = scene(Scene::Trouble);
    let states: Vec<Resource> = DECK_RESOURCES
        .iter()
        .copied()
        .filter(|&r| words::state(r, &snap, &MAC).is_some())
        .collect();
    assert!(states.len() >= 3, "CONTROL: {states:?}");
    let mut said = 0;
    for kind in [FullKind::Mid, FullKind::Narrow, FullKind::Slim] {
        let w = full_width(kind, &m);
        let at = format!("{kind:?} at {w} pt");
        let p = plan::plan_full(w, &m);
        let laid = laid(Scene::Trouble, w, 2.0, DeckForm::Full, &m, false);
        if p.details.is_some() {
            continue;
        }
        let col = p.amount.unwrap_or_else(|| panic!("{at}: an amount column"));
        for (i, &res) in DECK_RESOURCES.iter().enumerate() {
            if states.contains(&res) {
                rule_s(&laid, &m, &snap, res, col, p.rows, i as u32, &at);
                said += 1;
            }
        }
    }
    for kind in BlockKind::ALL {
        let w = folded_width(kind, &m);
        let at = format!("{kind:?} at {w} pt");
        let p = plan::plan_folded(w, &m);
        let laid = laid(Scene::Trouble, w, 2.0, DeckForm::Folded, &m, false);
        let head = laid
            .head
            .as_ref()
            .unwrap_or_else(|| panic!("{at}: a header"));
        for (bi, b) in p.blocks.iter().enumerate() {
            let ours: Vec<(u32, Resource)> = DECK_RESOURCES
                .iter()
                .copied()
                .skip(bi * 4)
                .take(4)
                .zip(0_u32..)
                .map(|(r, i)| (i, r))
                .collect();
            match (b.details, b.amount) {
                (Some(details), _) => {
                    for &(r, res) in &ours {
                        let Some(st) = words::state(res, &snap, &MAC) else {
                            continue;
                        };
                        let top = b.rows.top(r);
                        let drawn = texts_in(&laid, details, top - 1.0, top + b.rows.cell + 1.0);
                        let kept = words::details(res, &snap, &MAC)
                            .into_iter()
                            .find(|c| c.keep)
                            .map(|c| c.forms)
                            .unwrap_or_default();
                        assert!(
                            drawn.first().is_some_and(|d| kept.contains(d)),
                            "{at}: {res:?} ({:?}) draws {drawn:?}, keeps {kept:?}",
                            st.state
                        );
                        said += 1;
                    }
                }
                (None, Some(amount)) => {
                    for &(r, res) in &ours {
                        if states.contains(&res) {
                            rule_s(&laid, &m, &snap, res, amount, b.rows, r, &at);
                            said += 1;
                        }
                    }
                }
                (None, None) => {
                    let worst = ours
                        .iter()
                        .filter_map(|&(_, res)| words::state(res, &snap, &MAC).map(|st| (res, st)))
                        .reduce(|a, b| {
                            let rank =
                                |st: &words::StateWords| (st.state, st.mark == words::Mark::Low);
                            if rank(&b.1) > rank(&a.1) { b } else { a }
                        });
                    let label = head
                        .labels
                        .iter()
                        .find(|l| l.label == Label::State && (l.x - b.name.x).abs() < EPS);
                    match worst {
                        Some((res, st)) => {
                            let name = words::resource_name(res);
                            let named = res == Resource::Processes || st.label.starts_with(name);
                            assert!(named, "{at}: {res:?} says {:?}", st.label);
                            assert!(
                                label.is_some_and(|l| l.text.starts_with(&st.label)),
                                "{at}: block {bi} says {:?}: {label:?}",
                                st.label
                            );
                            said += 1;
                        }
                        None => assert!(label.is_none(), "{at}: {label:?}"),
                    }
                }
            }
        }
    }
    assert!(said >= 10, "CONTROL: {said} states read");
}

fn hit_at(hits: &[Hit], x: f64, y: f64, s: f64) -> Option<HitKind> {
    let (x, y) = ((x * s) as f32, (y * s) as f32);
    hits.iter()
        .find(|h| {
            let (hx, hy, hw, hh) = h.rect;
            x >= hx && x < hx + hw && y >= hy && y < hy + hh
        })
        .map(|h| h.kind)
}

fn row_hit(res: Resource, bar: Bar) -> Option<HitKind> {
    Some(HitKind::Row(Instrument::of(res), bar))
}

/// Every column of a row names the row.
#[expect(clippy::too_many_arguments, reason = "one row's columns and band")]
fn check_row(
    hits: &[Hit],
    s: f64,
    res: Resource,
    rows: Rows,
    r: u32,
    cols: &[Col],
    scopes: Option<(Col, Col)>,
    at: &str,
) {
    let top = rows.top(r);
    let gap = rows.pitch - rows.cell;
    let mid = top + rows.cell / 2.0;
    for col in cols {
        let x = col.x + col.w / 2.0;
        assert_eq!(
            hit_at(hits, x, mid, s),
            row_hit(res, Bar::Global),
            "{at}: {res:?} at {x}"
        );
    }
    let x = cols.first().map_or(0.0, |c| c.x + 1.0);
    let inset = 0.6 / s;
    for y in [top - gap / 2.0 + inset, top + rows.cell + gap / 2.0 - inset] {
        assert_eq!(
            hit_at(hits, x, y, s),
            row_hit(res, Bar::Global),
            "{at}: {res:?} at y {y}"
        );
    }
    if let Some((aterm, tab)) = scopes {
        for (col, bar) in [(aterm, Bar::Aterm), (tab, Bar::Tab)] {
            let x = col.x + col.w / 2.0;
            assert_eq!(
                hit_at(hits, x, mid, s),
                row_hit(res, bar),
                "{at}: {res:?} {bar:?}"
            );
        }
    }
}

fn strip_col(strip: Strip) -> Col {
    Col {
        x: strip.x,
        w: strip.end() - strip.x,
    }
}

#[test]
fn hit_map() {
    let m = FaceMetrics::helvetica_neue();
    for (w, s) in [(1119.0, 2.0), (900.0, 2.0), (550.0, 2.0), (350.0, 1.0)] {
        let p = plan::plan_full(w, &m);
        let at = format!("Full {:?} at {w} pt, {s}×", p.kind);
        let laid = laid(Scene::Calm, w, s, DeckForm::Full, &m, true);
        let hits = &laid.hits;
        let mut cols = vec![p.name, strip_col(p.strip)];
        cols.extend([p.pct, p.bar, p.amount, p.details].into_iter().flatten());
        let scopes = p.aterm.zip(p.tab);
        for (i, &res) in DECK_RESOURCES.iter().enumerate() {
            check_row(hits, s, res, p.rows, i as u32, &cols, scopes, &at);
        }
        let header = hit_at(hits, p.strip.x + 20.0, 6.0, s);
        assert_eq!(header, Some(HitKind::Header), "{at}");
        let cores = match p.cores {
            CoresAt::Block { at, .. } => hit_at(hits, at.x + 2.0, 40.0, s),
            CoresAt::Row(room) => {
                let y = p.rows.top(DECK_RESOURCES.len() as u32) + p.rows.cell / 2.0;
                hit_at(hits, room.x + 2.0, y, s)
            }
        };
        assert_eq!(cores, Some(HitKind::Cores), "{at}");
    }
    let header_names = laid(Scene::Calm, 1119.0, 2.0, DeckForm::Full, &m, true)
        .hits
        .iter()
        .find(|h| h.kind == HitKind::Header)
        .map(|h| h.instrument());
    assert_eq!(header_names, Some(Instrument::HowToRead));
    for (w, s) in [(1119.0, 2.0), (750.0, 2.0), (450.0, 1.0)] {
        let p = plan::plan_folded(w, &m);
        let at = format!("Folded {:?} at {w} pt, {s}×", p.blocks[0].kind);
        let laid = laid(Scene::Calm, w, s, DeckForm::Folded, &m, true);
        let hits = &laid.hits;
        for (bi, b) in p.blocks.iter().enumerate() {
            let mut cols = vec![b.name, strip_col(b.strip)];
            cols.extend([b.pct, b.bar, b.amount, b.details].into_iter().flatten());
            let scopes = b.aterm.zip(b.tab);
            for (res, r) in DECK_RESOURCES
                .iter()
                .copied()
                .skip(bi * 4)
                .take(4)
                .zip(0_u32..)
            {
                check_row(hits, s, res, b.rows, r, &cols, scopes, &at);
            }
            if bi == 1 {
                let y = b.rows.top(3) + b.rows.cell / 2.0;
                assert_eq!(
                    hit_at(hits, b.name.x + 1.0, y, s),
                    Some(HitKind::Cores),
                    "{at}"
                );
            }
        }
        let y = p.blocks[0].rows.top(1) + 4.0;
        let gutter = (p.blocks[0].at.end() + p.blocks[1].at.x) / 2.0;
        assert_eq!(hit_at(hits, gutter, y, s), None, "{at}: the gutter");
        assert_eq!(
            hit_at(hits, p.blocks[0].strip.x + 20.0, 5.0, s),
            Some(HitKind::Header),
            "{at}"
        );
    }
    let w = folded_width(BlockKind::Slim, &m);
    let laid = laid(Scene::Trouble, w, 2.0, DeckForm::Folded, &m, true);
    let head = laid.head.as_ref().expect("a header");
    let labels: Vec<_> = head
        .labels
        .iter()
        .filter(|l| l.label == Label::State)
        .collect();
    assert!(
        !labels.is_empty(),
        "CONTROL: fslim at {w} pt has a state label"
    );
    for label in labels {
        let res = DECK_RESOURCES
            .iter()
            .copied()
            .find(|&r| {
                words::state(r, &scene(Scene::Trouble), &MAC)
                    .is_some_and(|st| label.text.starts_with(&st.label))
            })
            .expect("the label says a row's state");
        assert_eq!(
            hit_at(&laid.hits, label.x + 4.0, 6.0, 2.0),
            row_hit(res, Bar::Global),
            "{label:?}"
        );
    }
}

#[test]
#[ignore = "writes PNGs for a person to read: set ATERM_DECK_GALLERY to a directory"]
fn deck_gallery() {
    let Some(dir) = std::env::var_os("ATERM_DECK_GALLERY") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("the gallery directory");
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let scenes = [
        ("calm", Scene::Calm),
        ("trouble", Scene::Trouble),
        ("firstrun", Scene::FirstRun),
        ("harness_off", Scene::HarnessOff),
    ];
    let mut written = Vec::new();
    for (name, sc) in scenes {
        let snap = scene(sc);
        for (w, s) in [
            (1119.0, 2.0),
            (550.0, 2.0),
            (350.0, 2.0),
            (1119.0, 1.0),
            (450.0, 1.0),
        ] {
            for form in [DeckForm::Full, DeckForm::Folded] {
                for (ground, inks) in [("dark", super::tests::dark_inks()), ("light", light_inks())]
                {
                    let mut input = scene_input(&snap, form);
                    input.inks = inks;
                    let g = geom(w, s, form);
                    let (prims, _) = deck_paint(&g, &input);
                    let (rgba, pw, ph) = crate::tray_raster::rasterize_tray(
                        &prims,
                        g.w as u32,
                        g.h as u32,
                        1.0,
                        [0, 0, 0, 0],
                    );
                    let png = crate::app_introspect::encode_rgba8_png(&rgba, pw, ph)
                        .expect("the frame encodes");
                    let form = if form == DeckForm::Full {
                        "full"
                    } else {
                        "folded"
                    };
                    let file = dir.join(format!("{name}_{w}pt_{s}x_{form}_{ground}.png"));
                    std::fs::write(&file, png).expect("the PNG writes");
                    written.push(file);
                }
            }
        }
    }
    assert_eq!(written.len(), 4 * 5 * 2 * 2);
}
