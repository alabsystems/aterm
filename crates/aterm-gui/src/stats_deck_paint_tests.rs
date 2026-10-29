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
use aterm_vitals::{Cell, Filling, Rates, Reading, TrendView};

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

/// The readings behind `ring`'s last minute (its last cell the bucket holding `at`), one a
/// second as the probe takes them while a focused window shows the deck, the newest at `at`:
/// each its bucket's mean, over where its bucket is; none where its bucket was not read.
pub(crate) fn readings_of(ring: &[Cell], at: i64) -> Arc<[Reading]> {
    let last_k = at.div_euclid(10_000);
    (0..60_i64)
        .rev()
        .filter_map(|back| {
            let t = at - back * 1000;
            let back_k = usize::try_from(last_k - t.div_euclid(10_000)).ok()?;
            let cell = ring.get(ring.len().checked_sub(1 + back_k)?)?;
            Some(Reading {
                at_ms: t,
                v: cell.mean?,
                over: cell.over,
            })
        })
        .collect()
}

fn series(now: f64, capacity: f64) -> Series {
    Series {
        now: Some(now),
        capacity: Some(capacity),
        ring: Arc::from(Vec::new()),
        binds: false,
        ..Series::default()
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
        recent: readings_of(&ring, AT),
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
            measured: true,
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
            born_ms: None,
            unborn: 999,
        }
    } else {
        Busiest {
            name: "rustc".into(),
            cores: 5.9,
            n: 9,
            scope: Some(ProcScope::Tab(3)),
            born_ms: None,
            unborn: 9,
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

/// Built-in theme `name`'s band as the deck reads it: its ground, words and accent.
pub(crate) fn theme_band(name: &str) -> (Rgb, Rgb, Rgb) {
    let scheme = aterm_types::scheme::builtin(name).expect("a built-in theme");
    let band = crate::chrome_band::band_colors(crate::chrome_band::ChromePalette {
        theme: aterm_render::Theme::from(&scheme),
        ansi: Some(crate::chrome_band::ansi_of_scheme(&scheme)),
        authored: aterm_messages::tokens::AuthoredChrome::NONE,
    });
    (band.bar_bg, band.value, band.accent)
}

/// The inks the deck solves on built-in theme `name`'s band, as a window drawing it does.
pub(crate) fn theme_inks(name: &str) -> VitalsInks {
    let (ground, words, accent) = theme_band(name);
    ink::vitals_inks(ground, words, accent)
}

pub(crate) fn scene_input(snap: &Snapshot, form: DeckForm) -> DeckInput<'_> {
    let long = snap.tab(FRONT).is_some_and(|t| t.label == LONG_TAB);
    DeckInput {
        network: None,
        network_target: crate::network_deck::Target::Overview,
        snap: Some(snap),
        prev_snap: None,
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
        live: crate::stats_live::DeckLive::STILL,
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
    /// The now marker's widest beat ring meets no words and no mark ([`beat_faults`]).
    Beat,
    /// Every row that prints a % draws its three-scope bar ([`bar_faults`]).
    Bars,
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
                    .chain(tagged(Check::Beat, beat_faults(&d.laid.boxes, BEAT_RING)))
                    .chain(tagged(Check::Bars, bar_faults(d)))
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
                    [
                        Check::Whole,
                        Check::Key,
                        Check::Cells,
                        Check::Beat,
                        Check::Bars,
                    ]
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

/// A table's rows and its %, bar-column and bar-under columns, as [`bar_faults`] reads them.
type BarTable = (Rows, Option<Col>, Option<Col>, Option<Col>);

/// The tables a layout at `w` draws.
fn bar_tables(form: DeckForm, w: f64, m: &FaceMetrics) -> Vec<BarTable> {
    match form {
        DeckForm::Full => {
            let p = plan::plan_full(w, m);
            let t = (p.rows, p.pct, p.meter.bar, p.bar_under());
            if p.ground_only { Vec::new() } else { vec![t] }
        }
        DeckForm::Folded => {
            let p = plan::plan_folded(w, m);
            if p.ground_only {
                return Vec::new();
            }
            p.blocks
                .iter()
                .map(|b| (b.rows, b.pct, b.meter.bar, b.bar_under()))
                .collect()
        }
    }
}

/// Capacity occupies the history/amount span only for RAM/Disk without an amount column.
/// Those rows keep their scope bar under the percentage; GPU keeps its ordinary scope bar.
fn capacity_under(form: DeckForm, w: f64, table: usize, res: Resource, m: &FaceMetrics) -> bool {
    matches!(res, Resource::Memory | Resource::Disk)
        && match form {
            DeckForm::Full => plan::plan_full(w, m).meter.run.is_none(),
            DeckForm::Folded => plan::plan_folded(w, m).blocks[table].meter.run.is_none(),
        }
}

/// Where a row prints a % and draws no three-scope bar
/// (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §5): in the column right of its strip where
/// one fits, else under the figure, as drawn: the % column's width on whole device px, its top
/// [`plan::UNDER_GAP`] or more below the digits' baseline as it falls on the device px, and its
/// foot a device px or more short of the next row's top.
fn bar_faults(d: &Swept<'_>) -> Vec<String> {
    let mut why = Vec::new();
    let boxes = &d.laid.boxes;
    for (ti, (rows, pct, bar, under)) in bar_tables(d.form, d.w, d.m).into_iter().enumerate() {
        let Some(pct) = pct else { continue };
        for r in 0..rows.count {
            let first = if d.form == DeckForm::Full { 0 } else { ti * 4 };
            let Some(&res) = DECK_RESOURCES.get(first + r as usize) else {
                continue;
            };
            let (bar, under) = if capacity_under(d.form, d.w, ti, res, d.m) {
                (None, Some(pct))
            } else {
                (bar, under)
            };
            let base = rows.top(r) + rows.baseline;
            let Some(figure) = boxes.iter().find(|b| {
                b.kind == BoxKind::Text
                    && b.room == Some((pct.x, pct.end()))
                    && (b.r[3] - base).abs() < EPS
            }) else {
                continue;
            };
            if !figure.text.ends_with('%') {
                continue;
            }
            let drawn = |want: [f64; 4]| {
                boxes.iter().any(|b| {
                    b.kind == BoxKind::Bar && b.r.iter().zip(want).all(|(a, w)| (a - w).abs() < EPS)
                })
            };
            let ok = match (bar, under) {
                (Some(col), None) => {
                    let top = rows.top(r) + (rows.cell - rows.bar_h) / 2.0;
                    drawn([col.x, top, col.end(), top + rows.bar_h])
                }
                (None, Some(col)) => {
                    let s = d.s;
                    let pt = |v: f32| f64::from(v) / s;
                    let top = pt(px(base, s) + px(plan::UNDER_GAP, s));
                    let foot = pt(px(rows.top(r + 1), s) - 1.0);
                    let (x0, x1) = (pt(px(col.x, s)), pt(px(col.end(), s)));
                    boxes.iter().any(|b| {
                        b.kind == BoxKind::Bar
                            && (b.r[0] - x0).abs() < EPS
                            && (b.r[2] - x1).abs() < EPS
                            && b.r[1] >= top - EPS
                            && b.r[3] <= foot + EPS
                            && b.r[3] > b.r[1]
                    })
                }
                _ => false,
            };
            if !ok {
                why.push(format!(
                    "row {r} at {:?} prints {:?} without its bar (column {bar:?}, under {under:?})",
                    pct, figure.text
                ));
            }
        }
    }
    why
}

/// Every row that prints a % draws its scope bar, in every form, at every width, on both faces
/// at 1× and 2×: in its column right of the strip where it has one, else under the figure, inside
/// the row's pitch (`docs/DESIGN-stats-deck-meter-2026-10-26.md` §2).
#[test]
fn every_row_with_a_percent_draws_its_bar() {
    let failed = swept(Check::Bars);
    assert!(failed.is_empty(), "{}", failed.join("\n"));
    let m = FaceMetrics::helvetica_neue();
    for (form, w) in [
        (DeckForm::Folded, 1119.0),
        (DeckForm::Folded, 650.0),
        (DeckForm::Folded, 600.0),
        (DeckForm::Folded, 350.0),
        (DeckForm::Full, 580.0),
        (DeckForm::Full, 400.0),
        (DeckForm::Full, 350.0),
        (DeckForm::Full, 210.0),
    ] {
        let tables = bar_tables(form, w, &m);
        let laid = laid(Scene::Calm, w, 2.0, form, &m, false);
        let (rows, pct, bar, bar_under) = tables[0];
        assert!(
            pct.is_some(),
            "CONTROL {form:?} {w} pt: percentages present"
        );
        for (r, &res) in DECK_RESOURCES.iter().take(rows.count as usize).enumerate() {
            let r = r as u32;
            let col = if capacity_under(form, w, 0, res, &m) {
                pct
            } else {
                bar.or(bar_under)
            }
            .expect("CONTROL: the resource's bar column");
            let bars = laid
                .boxes
                .iter()
                // Under-percent bars note the whole device-pixel rect they actually draw.
                .filter(|b| {
                    b.kind == BoxKind::Bar
                        && (b.r[0] - col.x).abs() <= 0.25
                        && b.r[1] >= rows.top(r)
                        && b.r[1] < rows.top(r + 1)
                })
                .count();
            assert_eq!(
                bars, 1,
                "CONTROL {form:?} {w} pt {res:?}: its bar at {col:?}"
            );
        }
    }
}

/// The panels drawn from `x` with their top in `y0..y1`, device px.
fn panels_at(prims: &[DrawPrim], x: f32, (y0, y1): (f32, f32)) -> Vec<(Rect, [u8; 4])> {
    prims
        .iter()
        .filter_map(|p| match *p {
            DrawPrim::Panel {
                x: px0,
                y,
                w,
                h,
                fill,
                ..
            } if (px0 - x).abs() < 0.01 && y >= y0 && y < y1 => Some(((px0, y, w, h), fill)),
            _ => None,
        })
        .collect()
}

/// The bar under a % figure, as drawn (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §5):
/// the % column's width on whole device px, up to [`plan::UNDER_H`] tall, its top
/// [`plan::UNDER_GAP`] or more below the digits'
/// baseline as it falls on the device px, so it never meets their overshoot, and its foot a
/// device px or more short of the next row's top and clear of its ink, so it never meets that
/// row's cells or digits;
/// every fill nested from its left edge, a scope above zero at
/// least a device px; the column bar's own inks in both themes and under Increase Contrast; and
/// a click anywhere on it opens its row's lens, as the bar's column does, though its foot stands
/// past the row's band.
#[test]
fn the_bar_under_the_percent_is_the_bars_on_whole_pixels() {
    let m = FaceMetrics::helvetica_neue();
    let snap = scene(Scene::Calm);
    let mut nested = 0;
    for (form, w, wide) in [
        (DeckForm::Folded, 350.0, 1119.0),
        (DeckForm::Folded, 400.0, 1119.0),
        (DeckForm::Folded, 450.0, 1119.0),
        (DeckForm::Full, 210.0, 580.0),
    ] {
        for s in [1.0, 2.0] {
            for (theme, inks) in [("dark", super::tests::dark_inks()), ("light", light_inks())] {
                for contrast in [false, true] {
                    let at = format!("{form:?} {w} pt {s}× {theme} contrast {contrast}");
                    let laid_at = |w: f64| {
                        let mut input = scene_input(&snap, form);
                        input.inks = inks;
                        input.contrast = contrast;
                        let words = DeckWords::new(&input, &m);
                        lay(&geom(w, s, form), &input, &words, &m, true)
                    };
                    let (narrow, column) = (laid_at(w), laid_at(wide));
                    let ink = |c: Rgb| rgba(c, 0xFF);
                    let scope_inks = if contrast {
                        [inks.machine_ic, inks.aterm_ic, inks.tab_ic]
                    } else {
                        [inks.machine, inks.aterm, inks.tab]
                    };
                    let bar_inks: Vec<[u8; 4]> = std::iter::once(inks.track)
                        .chain(scope_inks)
                        .map(ink)
                        .collect();
                    let tables = bar_tables(form, w, &m);
                    let wide_tables = bar_tables(form, wide, &m);
                    for (ti, &(rows, _, _, under)) in tables.iter().enumerate() {
                        let col = under.unwrap_or_else(|| panic!("CONTROL {at}: bars under the %"));
                        let (wide_rows, wide_pct, wide_bar, wide_under) = wide_tables[ti];
                        let first = if form == DeckForm::Full { 0 } else { ti * 4 };
                        for r in 0..rows.count {
                            let Some(&res) = DECK_RESOURCES.get(first + r as usize) else {
                                continue;
                            };
                            let at = format!("{at}, {res:?}");
                            let base = px(rows.top(r) + rows.baseline, s);
                            let next = px(rows.top(r + 1), s);
                            let drawn = panels_at(
                                &narrow.prims,
                                px(col.x, s),
                                (base + px(plan::UNDER_GAP, s), next),
                            );
                            let Some(&((x, y, tw, h), _)) = drawn.first() else {
                                panic!("{at}: no bar under the %");
                            };
                            assert_eq!(
                                (x, tw),
                                (px(col.x, s), px(col.end(), s) - px(col.x, s)),
                                "{at}: the track"
                            );
                            assert!(
                                (1.0..=px(plan::UNDER_H, s)).contains(&h),
                                "{at}: positive bounded line height {h}"
                            );
                            let next_ink =
                                ((rows.top(r + 1) + rows.baseline - ASCENT_EM * rows.name_pt) * s)
                                    .floor() as f32;
                            assert!(
                                y + h <= next - 1.0 && y + h <= next_ink,
                                "{at}: {y} + {h} meets the next row at {next}, ink at {next_ink}"
                            );
                            let mut fills = Vec::new();
                            for &((fx, fy, fw, fh), fill) in &drawn {
                                assert!(
                                    [fx, fy, fw, fh].iter().all(|v| v.fract() == 0.0),
                                    "{at}: {fx} {fy} {fw} {fh} off whole px"
                                );
                                assert_eq!((fx, fy, fh), (x, y, h), "{at}: a fill off the line");
                                assert!((1.0..=tw).contains(&fw), "{at}: a fill {fw} px of {tw}");
                                assert!(bar_inks.contains(&fill), "{at}: {fill:?} is no bar ink");
                                fills.push((fw, fill));
                            }
                            // Nested: each fill drawn after the track is no wider than the one
                            // before it.
                            assert!(
                                fills.windows(2).skip(1).all(|p| p[1].0 <= p[0].0),
                                "{at}: {fills:?}"
                            );
                            nested += usize::from(fills.len() == 4);
                            // The same inks in the wide layout, including a capacity row
                            // whose wide layout also keeps its bar under the percentage.
                            let wide_moved = capacity_under(form, wide, ti, res, &m);
                            let wide_col = if wide_moved {
                                wide_pct
                            } else {
                                wide_bar.or(wide_under)
                            }
                            .expect("CONTROL: the wide resource's bar");
                            let band = if wide_moved || wide_bar.is_none() {
                                (
                                    px(wide_rows.top(r) + wide_rows.baseline + plan::UNDER_GAP, s),
                                    px(wide_rows.top(r + 1), s),
                                )
                            } else {
                                (
                                    px(wide_rows.top(r), s),
                                    px(wide_rows.top(r) + wide_rows.cell, s),
                                )
                            };
                            let mut theirs: Vec<[u8; 4]> =
                                panels_at(&column.prims, px(wide_col.x, s), band)
                                    .into_iter()
                                    .map(|(_, fill)| fill)
                                    .filter(|f| bar_inks.contains(f))
                                    .collect();
                            let mut ours: Vec<[u8; 4]> = fills.iter().map(|f| f.1).collect();
                            theirs.sort_unstable();
                            ours.sort_unstable();
                            assert_eq!(ours, theirs, "{at}: the column's inks");
                            // A click anywhere on it, its foot past the row's band included,
                            // opens the row's lens; past it the next row's band takes over.
                            let mid = x + tw / 2.0;
                            for py in [y + 0.5, y + h - 0.5] {
                                let hit =
                                    hit_at(&narrow.hits, f64::from(mid) / s, f64::from(py) / s, s);
                                if capacity_under(form, w, ti, res, &m) {
                                    assert!(
                                        matches!(hit, Some(HitKind::Row(inst, _)) if inst == Instrument::of(res)),
                                        "{at}: a click at {py} px names its resource: {hit:?}"
                                    );
                                } else {
                                    assert_eq!(
                                        hit,
                                        row_hit(res, Bar::Global),
                                        "{at}: a click at {py} px"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(nested > 20, "CONTROL: {nested} bars draw all three scopes");
}

#[test]
fn no_text_overlaps_and_none_leaves_the_card() {
    let failed = swept(Check::Whole);
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// How far the now marker's beat reaches from the dot's centre, pt: the dot and the ring's whole
/// widening.
const BEAT_RING: f64 = crate::stats_live::DOT_R + crate::stats_live::BEAT_REACH;

/// Where a ring of radius `reach` round each now dot (its `Mark` box "now") meets a word, a
/// swatch or another mark among `boxes`: T-G4's box for the dot is the dot alone, so the sweep did
/// not see the beat's ring cross the `now` beside it at every width (the review of 2026-10-26).
fn beat_faults(boxes: &[Boxed], reach: f64) -> Vec<String> {
    let mut why = Vec::new();
    for dot in boxes
        .iter()
        .filter(|b| b.kind == BoxKind::Mark && b.text == "now")
    {
        let (cx, cy) = ((dot.r[0] + dot.r[2]) / 2.0, (dot.r[1] + dot.r[3]) / 2.0);
        for b in boxes {
            if std::ptr::eq(b, dot)
                || !matches!(b.kind, BoxKind::Text | BoxKind::Swatch | BoxKind::Mark)
            {
                continue;
            }
            let dx = (b.r[0] - cx).max(cx - b.r[2]).max(0.0);
            let dy = (b.r[1] - cy).max(cy - b.r[3]).max(0.0);
            if dx.hypot(dy) < reach - EPS {
                why.push(format!(
                    "the beat round ({cx}, {cy}) meets {:?} {:?} {:?}",
                    b.kind, b.text, b.r
                ));
            }
        }
    }
    why
}

/// The now dot's beat, widest, meets no words and no mark at any width, in either form or face,
/// at 1× or 2× (the % column ends `PCT_TO_STRIP` before the strip, past the ring's reach).
#[test]
fn the_beat_ring_meets_no_words() {
    let failed = swept(Check::Beat);
    assert!(failed.is_empty(), "{}", failed.join("\n"));
    // CONTROL: the check sees the `now` beside the dot, a point past the ring.
    let m = deck_metrics();
    let d = laid(Scene::Calm, 600.0, 2.0, DeckForm::Folded, &m, false);
    let dots = d
        .boxes
        .iter()
        .filter(|b| b.kind == BoxKind::Mark && b.text == "now")
        .count();
    assert_eq!(dots, 2, "CONTROL: a dot over each block's strip");
    assert_eq!(beat_faults(&d.boxes, BEAT_RING), [""; 0]);
    let wider = beat_faults(&d.boxes, BEAT_RING + 1.5);
    assert!(
        wider.iter().any(|why| why.contains("Text \"now\"")),
        "CONTROL: {wider:?}"
    );
}

fn key(d: &Swept<'_>) -> Vec<String> {
    let mut why = Vec::new();
    let Some(head) = d.laid.head.as_ref() else {
        return why;
    };
    let strips = true;
    let over_w = KeyItem::Over.width(d.m);
    let fits = strips && head.key.spans.iter().any(|s| s.x1 - s.x0 >= over_w - EPS);
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
    // Where every table's state stands over its names, the key may lose its room to them.
    let tables = if d.form == DeckForm::Full { 1 } else { 2 };
    let all_states = head
        .labels
        .iter()
        .filter(|l| matches!(l.label, Label::State(_)))
        .count()
        == tables;
    if !fits && !all_states && strips {
        why.push(format!("no span fits \"over max\": {:?}", head.key.spans));
    }
    if !strips && drawn("over max") > 0 {
        why.push("\"over max\" keys no strip".into());
    }
    // The two blocks share one time axis: their strips span the same seconds, and its label
    // stands once.
    let time = head
        .labels
        .iter()
        .find(|l| l.label == crate::stats_plan::Label::Time)
        .map_or(0, |l| drawn(&l.text));
    if d.form == DeckForm::Folded && time != usize::from(strips) {
        why.push(format!("the time label {time} times"));
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

/// The narrowest Full deck whose Details column is `w` pt or wider.
fn full_with_details(w: f64, m: &FaceMetrics) -> f64 {
    (600..=6000)
        .map(f64::from)
        .find(|&deck| plan::plan_full(deck, m).details.is_some_and(|d| d.w >= w))
        .unwrap_or_else(|| panic!("no Full deck has {w} pt of Details"))
}

/// The CPU's Details fold by their column's width: its tab off the busiest clause first, then
/// the cause to its short form, then the busiest clause before the load.
#[test]
fn the_cause_clause_folds_before_load_and_after_busiest() {
    let m = FaceMetrics::helvetica_neue();
    let sep = " \u{b7} ";
    let [wide_w, mid_w, between_w, owner_w] =
        [487.0, 242.0, 183.0, 162.0].map(|d| full_with_details(d, &m));
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
    let (wide, marks) = drawn(Scene::Trouble, wide_w, DeckForm::Full).expect("Details");
    assert_eq!(
        wide,
        [
            "8 waiting for a core",
            sep,
            "Spotlight, after start-up",
            sep,
            "rustc \u{d7}9 in Tab 3",
        ]
    );
    let cause: Vec<&Boxed> = marks.iter().filter(|b| b.text == "cause").collect();
    assert_eq!(cause.len(), 1, "one \u{2235}: {marks:?}");
    let full = laid(Scene::Trouble, wide_w, 2.0, DeckForm::Full, &m, true);
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

    let (mid, _) = drawn(Scene::Trouble, mid_w, DeckForm::Full).expect("Details");
    assert_eq!(
        mid,
        [
            "8 waiting for a core",
            sep,
            "Spotlight, after start-up",
            sep,
            "rustc \u{d7}9"
        ],
        "the busiest clause sheds its tab first"
    );
    let (between, _) = drawn(Scene::Trouble, between_w, DeckForm::Full).expect("Details");
    assert_eq!(
        between,
        [
            "8 waiting for a core",
            sep,
            "Spotlight",
            sep,
            "rustc \u{d7}9"
        ],
        "then the cause shortens"
    );
    let (owner, _) = drawn(Scene::Trouble, owner_w, DeckForm::Full).expect("Details");
    assert_eq!(
        owner,
        ["8 waiting for a core", sep, "Spotlight"],
        "and busiest folds before the load"
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
        .filter(|b| {
            matches!(
                b.kind,
                BoxKind::Strip | BoxKind::Bar | BoxKind::Core | BoxKind::Gauge
            )
        })
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

/// The disk's run is its used space with the maximum (`1.11 of 2.00 TB used`), where a run stands;
/// freed is a standing fact the lens keeps, never the deck.
#[test]
fn freed_stays_out_of_the_deck() {
    let m = FaceMetrics::helvetica_neue();
    for (w, form, run) in [
        (2560.0, DeckForm::Folded, true),
        (1424.0, DeckForm::Folded, false),
        (1039.0, DeckForm::Folded, false),
        (1039.0, DeckForm::Full, true),
        (700.0, DeckForm::Full, true),
    ] {
        for s in [1.0, 2.0] {
            let laid = laid(Scene::Calm, w, s, form, &m, false);
            let said = |t: &str| {
                laid.boxes
                    .iter()
                    .any(|b| b.kind == BoxKind::Text && b.text == t)
            };
            assert_eq!(
                said("1.11 of 2.00 TB used") || said("1.11/2.00 TB used"),
                run,
                "{form:?} {w} pt at {s}×"
            );
            assert!(
                !said("1.11 TB used"),
                "{form:?} {w} pt at {s}×: a run without its maximum"
            );
            let freed = laid.boxes.iter().any(|b| b.text.contains("freed"));
            assert!(!freed, "{form:?} {w} pt at {s}×");
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
    let first = scene(Scene::FirstRun);
    // A Folded strip draws a reading a second: punch a 16 s hole in every row's readings, past
    // what a later reading may speak for, so its slots stand unread.
    let mut holed = first.clone();
    for view in &mut holed.resources {
        let kept: Vec<Reading> = view
            .recent
            .iter()
            .copied()
            .filter(|r| !(AT - 18_000 < r.at_ms && r.at_ms < AT - 2_000))
            .collect();
        view.recent = kept.into();
    }
    let inks = super::tests::dark_inks();
    let heat: Vec<[u8; 4]> = inks
        .heat
        .iter()
        .chain(&inks.heat_ic)
        .map(|&c| rgba(c, 0xFF))
        .collect();
    let hollow = rgba(inks.hollow, 0xFF);
    let (mut outlines, mut ticks, mut unread) = (0, 0, [0_usize; 2]);
    for (form, w, s) in [
        (DeckForm::Full, 1119.0, 2.0),
        (DeckForm::Full, 1119.0, 1.0),
        (DeckForm::Full, 350.0, 1.0),
        (DeckForm::Full, 180.0, 1.0),
        (DeckForm::Folded, 1119.0, 2.0),
        (DeckForm::Folded, 550.0, 1.0),
    ] {
        let snap = if form == DeckForm::Folded || w <= 200.0 {
            &holed
        } else {
            &first
        };
        let input = scene_input(snap, form);
        let words = DeckWords::new(&input, &m);
        let laid = lay(&geom(w, s, form), &input, &words, &m, true);
        for (strip, rows, at) in strips(w, form, &m) {
            let rects = multi_scale_slot_rects(&strip, s, 0.0);
            for (r, res) in at {
                if words::strip_words(res, snap).is_some()
                    || matches!(res, Resource::Gpu | Resource::Memory | Resource::Disk)
                {
                    continue;
                }
                let view = &snap.resources[res.index()];
                let slots = view.heat_multi_scale(strip.spec, AT, SLOT_COVER_MS, &snap.time_gaps);
                let (y, h) = (px(rows.top(r), s), px(rows.cell, s));
                for &(i, x, cw, is_live_now) in &rects {
                    let Some(slot_data) = slots.get(i) else {
                        continue;
                    };
                    let has_heat = if is_live_now {
                        view.heat_now.or(slot_data.cell.heat).is_some()
                    } else {
                        slot_data.cell.heat.is_some() || slot_data.cell.gap.is_some()
                    };
                    if has_heat {
                        continue;
                    }
                    let at = format!("{form:?} {w} pt at {s}×, {res:?} slot {i}");
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
                    unread[usize::from(form == DeckForm::Folded)] += 1;
                }
            }
        }
    }
    assert!(
        outlines > 0 && ticks > 0,
        "CONTROL: both forms drawn ({outlines}, {ticks})"
    );
    assert!(
        unread.iter().all(|&n| n > 0),
        "CONTROL: each form drew an unread column or slot ({unread:?})"
    );
}

/// The scale-band gridlines (`docs/DESIGN-stats-deck-owner-2026-10-26.md` §4: the gridline in the
/// gap before each band's first box marks every size's start): on each preset, at the narrowest
/// width each form lays it, at 1× and 2×, one 1 px column in the grid ink stands in the gap
/// before every band's first box past the live box, the live box's own edge included, at that
/// box's left edge less the gap, down every history surface with caption holes left clear, and
/// none stands elsewhere on the strip.
/// The bands come from the spec, so neighbouring bands whose boxes share a width keep theirs:
/// the 6 h preset's 10 s and 1 min blocks, the 4 h one's 1 s, 10 s and 1 min blocks and its
/// 10 min and 1 h ones, and the 6 min one's 1 s and 10 s blocks.
#[test]
fn a_gridline_stands_before_every_bands_first_box() {
    let m = FaceMetrics::helvetica_neue();
    let snap = scene(Scene::Calm);
    assert!(
        DECK_RESOURCES
            .iter()
            .all(|&res| words::strip_words(res, &snap).is_none()),
        "CONTROL: every Calm row draws its history"
    );
    let grid = rgba(super::tests::dark_inks().grid, 0xFF);
    let presets = [
        MultiScaleSpec::FULL_6H,
        MultiScaleSpec::FOLD_4H,
        MultiScaleSpec::COMPACT_6M,
        MultiScaleSpec::MINI_1M,
    ];
    let size = |ms: i64| match ms {
        ms if ms >= 3_600_000 => format!("{} h", ms / 3_600_000),
        ms if ms >= 60_000 => format!("{} min", ms / 60_000),
        ms => format!("{} s", ms / 1_000),
    };
    let (mut failed, mut columns, mut laid_presets) = (Vec::new(), [0_usize; 4], [false; 4]);
    for form in [DeckForm::Full, DeckForm::Folded] {
        let mut seen = [false; 4];
        for w in sweep_widths() {
            let (ground_only, spec) = match form {
                DeckForm::Full => {
                    let p = plan::plan_full(w, &m);
                    (p.ground_only, p.strip.spec)
                }
                DeckForm::Folded => {
                    let p = plan::plan_folded(w, &m);
                    (p.ground_only, p.blocks[0].strip.spec)
                }
            };
            let Some(pi) = presets.iter().position(|&p| p == spec) else {
                continue;
            };
            if ground_only || seen[pi] {
                continue;
            }
            seen[pi] = true;
            laid_presets[pi] = true;
            for s in [1.0, 2.0] {
                let input = scene_input(&snap, form);
                let words = DeckWords::new(&input, &m);
                let laid = lay(&geom(w, s, form), &input, &words, &m, true);
                for (bi, (strip, rows, at)) in strips(w, form, &m).into_iter().enumerate() {
                    let at_str = format!(
                        "{form:?} {w} pt at {s}×, strip {bi} ({} back)",
                        size(strip.spec.horizon_ms())
                    );
                    let (x0, x1) = (px(strip.x, s), px(strip.end(), s));
                    // Read the rendered history surfaces, not the old full-height row boxes:
                    // capacity captions retain a thin history below their words. An ordinary
                    // strip's note can include an overflow cap above the row's history.
                    let histories: Vec<_> = at
                        .iter()
                        .map(|&(r, res)| {
                            let surfaces: Vec<_> = laid
                                .boxes
                                .iter()
                                .filter(|b| {
                                    b.kind == BoxKind::Strip
                                        && b.r[0] == strip.x
                                        && b.r[2] == strip.end()
                                        && b.r[3] > rows.top(r)
                                        && b.r[3] <= rows.top(r + 1)
                                })
                                .collect();
                            assert_eq!(
                                surfaces.len(),
                                1,
                                "CONTROL {at_str}: {res:?} has its whole time axis"
                            );
                            let surface = surfaces[0];
                            let top = surface.r[1].max(rows.top(r));
                            assert!(
                                px(surface.r[3], s) > px(top, s),
                                "CONTROL {at_str}: {res:?} has positive history height"
                            );
                            (r, top, surface.r[3])
                        })
                        .collect();
                    let y0 = px(rows.top(0), s);
                    let y1 = px(histories.last().expect("resource rows").2, s);
                    let mut coverage = vec![1_usize; (y1 - y0) as usize];
                    // The caption clearance is half the planned gap in either form. Keep its
                    // whole band clear, including space between words and the thin history.
                    let clearance = (rows.pitch - rows.cell) / 2.0;
                    for (i, &(r, top, _)) in histories.iter().enumerate() {
                        if top > rows.top(r) {
                            let previous_foot =
                                i.checked_sub(1).map_or(y0, |i| px(histories[i].2, s));
                            let a = px(rows.top(r) - clearance, s).max(previous_foot);
                            let b = px(top, s);
                            coverage[(a - y0) as usize..(b - y0) as usize].fill(0);
                        }
                    }
                    for &(r, top, foot) in &histories {
                        assert!(
                            coverage[(px(top, s) - y0) as usize..(px(foot, s) - y0) as usize]
                                .iter()
                                .all(|&n| n == 1),
                            "CONTROL {at_str}: row {r}'s history remains covered"
                        );
                    }
                    let gap = px(1.0, s).max(1.0);
                    let rects = multi_scale_slot_rects(&strip, s, 0.0);
                    let slots = strip.spec.slots();
                    let left = |ix: usize| rects.iter().find(|r| r.0 == ix).map(|r| (r.1, r.2));
                    // Each band's first box past the live box, read off the spec's bands.
                    let mut want = Vec::new();
                    let mut first = 0;
                    for band in strip.spec.bands {
                        let ix = first;
                        first += usize::from(band.slots);
                        if band.live_now || band.slots == 0 {
                            continue;
                        }
                        let slot = slots[ix];
                        assert!(
                            !slot.is_live_now && slot.age_hi_ms - slot.age_lo_ms == band.period_ms,
                            "CONTROL {at_str}: slot {ix} is the {} band's first",
                            size(band.period_ms)
                        );
                        let (Some((bx, _)), Some((prev_x, prev_w))) =
                            (left(ix), ix.checked_sub(1).and_then(left))
                        else {
                            panic!("CONTROL {at_str}: slots {ix} and the one before it are laid");
                        };
                        let gx = bx - gap;
                        assert!(
                            prev_x + prev_w <= gx && gx + 1.0 <= bx,
                            "CONTROL {at_str}: {gx} px is in the gap before slot {ix}"
                        );
                        want.push((gx, band.period_ms));
                    }
                    columns[pi] += want.len();
                    let mut drawn: Vec<(f32, f32, f32)> = laid
                        .prims
                        .iter()
                        .filter_map(|p| match *p {
                            DrawPrim::Panel {
                                x, y, w, h, fill, ..
                            } if fill == grid
                                && w == 1.0
                                && (x0..x1).contains(&x)
                                && y < y1
                                && y + h > y0 =>
                            {
                                Some((x, y, h))
                            }
                            _ => None,
                        })
                        .collect();
                    for (gx, period) in want {
                        let mut actual = vec![0_usize; coverage.len()];
                        let mut found = false;
                        drawn.retain(|&(x, y, h)| {
                            if x != gx {
                                return true;
                            }
                            found = true;
                            if y < y0
                                || y + h > y1
                                || h <= 0.0
                                || y.fract() != 0.0
                                || h.fract() != 0.0
                            {
                                failed.push(format!(
                                    "{at_str}: gridline at {gx} px has invalid span {y}..{}",
                                    y + h
                                ));
                            } else {
                                for n in &mut actual[(y - y0) as usize..(y + h - y0) as usize] {
                                    *n += 1;
                                }
                            }
                            false
                        });
                        if !found {
                            failed.push(format!(
                                "{at_str}: no gridline at {gx} px, before the {} band's first box",
                                size(period)
                            ));
                            continue;
                        }
                        if actual != coverage {
                            failed.push(format!(
                                "{at_str}: gridline at {gx} px does not cover every history \
                                 exactly once while leaving caption holes clear: \
                                 actual {actual:?}, wanted {coverage:?}, rows {y0}..{y1}"
                            ));
                        }
                    }
                    failed.extend(drawn.into_iter().map(|(x, ..)| {
                        format!("{at_str}: a gridline at {x} px, before no band's first box")
                    }));
                }
            }
            if seen.iter().all(|&s| s) {
                break;
            }
        }
    }
    assert!(
        laid_presets.iter().all(|&p| p),
        "CONTROL: some form lays every preset ({laid_presets:?})"
    );
    assert!(
        failed.is_empty(),
        "{} faults over {columns:?} band starts (6 h, 4 h, 6 min, 1 min):\n{}",
        failed.len(),
        failed.join("\n")
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

/// Rule S on one row: with no Details, the figure run at the end of its bar says its state after
/// the reading, its clause before its word; the reading is never given up for it (review of
/// 2026-10-26: "under memory pressure the unit cell drops the maximum"). Where neither fits, the
/// run says the reading alone and the header the state.
fn rule_s(
    laid: &Laid,
    m: &FaceMetrics,
    snap: &Snapshot,
    res: Resource,
    (meter, rows, r): (plan::Meter, Rows, u32),
    at: &str,
) {
    let Some(st) = words::state(res, snap, &MAC) else {
        return;
    };
    let run = meter.run.unwrap_or_else(|| panic!("{at}: a figure run"));
    let pt = rows.amount_pt;
    let mut forms = words::monitor_amount(res, snap);
    if forms.is_empty() {
        forms = words::glance_run(res, &snap.resources[res.index()], snap, &MAC);
    }
    let top = rows.top(r);
    let drawn = texts_in(laid, run, top - 1.0, top + rows.cell + 1.0);
    let mut says: Vec<&str> = st.clauses.iter().map(String::as_str).collect();
    says.push(&st.word);
    let form = forms
        .iter()
        .find(|f| m.width(f, pt, false) <= run.w + EPS)
        .cloned();
    let used = form.as_ref().map_or(0.0, |f| m.width(f, pt, false) + 6.0);
    let said = says
        .iter()
        .find(|said| used + 1.1 * pt + m.width(said, pt, true) <= run.w + EPS);
    let want: Vec<String> = form
        .into_iter()
        .chain(said.map(|s| (*s).to_owned()))
        .collect();
    assert_eq!(drawn, want, "{at}: {res:?}");
    if said.is_none() {
        let head = laid.head.as_ref().expect("a header");
        assert!(
            head.labels
                .iter()
                .any(|l| matches!(l.label, Label::State(_))),
            "{at}: {res:?}: the header says the state"
        );
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
        if p.meter.run.is_none() {
            let head = laid
                .head
                .as_ref()
                .unwrap_or_else(|| panic!("{at}: a header"));
            said += usize::from(says_worst(head, &snap, &DECK_RESOURCES, 0, &at));
            continue;
        }
        for (i, &res) in DECK_RESOURCES.iter().enumerate() {
            if states.contains(&res) {
                rule_s(&laid, &m, &snap, res, (p.meter, p.rows, i as u32), &at);
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
            match (b.details, b.meter.run) {
                (Some(details), _) => {
                    for &(r, res) in &ours {
                        let Some(st) = words::state(res, &snap, &MAC) else {
                            continue;
                        };
                        let top = b.rows.top(r);
                        let drawn = texts_in(&laid, details, top - 1.0, top + b.rows.cell + 1.0);
                        let kept = words::glance_details(res, &snap, &[], &MAC)
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
                (None, Some(_)) => {
                    for &(r, res) in &ours {
                        if states.contains(&res) {
                            rule_s(&laid, &m, &snap, res, (b.meter, b.rows, r), &at);
                            said += 1;
                        }
                    }
                }
                (None, None) => {
                    let ours: Vec<Resource> = ours.iter().map(|&(_, res)| res).collect();
                    let block = u8::try_from(bi).expect("two blocks");
                    said += usize::from(says_worst(head, &snap, &ours, block, &at));
                }
            }
        }
    }
    assert!(said >= 10, "CONTROL: {said} states read");
}

/// Whether `head` says the worst state of the table `block`, whose rows are `ours`, over its
/// names (asserted where one is due); `false` where none is due.
fn says_worst(
    head: &plan::HeaderPlan,
    snap: &Snapshot,
    ours: &[Resource],
    block: u8,
    at: &str,
) -> bool {
    let worst = ours
        .iter()
        .filter_map(|&res| words::state(res, snap, &MAC).map(|st| (res, st)))
        .reduce(|a, b| {
            let rank = |st: &words::StateWords| (st.state, st.mark == words::Mark::Low);
            if rank(&b.1) > rank(&a.1) { b } else { a }
        });
    let label = head.labels.iter().find(|l| l.label == Label::State(block));
    let Some((res, st)) = worst else {
        assert!(label.is_none(), "{at}: {label:?}");
        return false;
    };
    let name = words::resource_name(res);
    let named = res == Resource::Processes || st.label.starts_with(name);
    assert!(named, "{at}: {res:?} says {:?}", st.label);
    // Whole, or cut to the room it has.
    assert!(
        label.is_some_and(|l| says_state(&l.text, &st.label)),
        "{at}: block {block} says {:?}: {label:?}",
        st.label
    );
    true
}

/// Whether a header's state words `text` say `label`: whole (with its `+n`), or cut to fit.
fn says_state(text: &str, label: &str) -> bool {
    text.starts_with(label)
        || text
            .strip_suffix('\u{2026}')
            .is_some_and(|cut| cut.len() >= 4 && label.starts_with(cut))
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

/// Every column of a row names the row, and its scope bar `bar` names this tab's segment, then
/// aterm's, then the machine's, left to right: where the row's bar has a tab's or aterm's fill a
/// click on it opens the lens on that scope (`docs/DESIGN-stats-deck-meter-2026-10-26.md` §3).
/// Answers which scopes' segments the row's bar named.
fn check_row(
    hits: &[Hit],
    s: f64,
    (res, rows, r): (Resource, Rows, u32),
    cols: &[Col],
    bar: Option<Col>,
    at: &str,
) -> [bool; 2] {
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
    let mut named = [false; 2];
    if let Some(bar) = bar {
        let order = |b: Bar| match b {
            Bar::Tab => 0,
            Bar::Aterm => 1,
            Bar::Global => 2,
        };
        let mut last = 0;
        let steps = (bar.w * 2.0 * s).floor() as u32;
        for i in 0..steps {
            let x = bar.x + (f64::from(i) + 0.5) / (2.0 * s);
            let Some(HitKind::Row(inst, b)) = hit_at(hits, x, mid, s) else {
                panic!("{at}: {res:?}'s bar at {x} names no row");
            };
            assert_eq!(inst, Instrument::of(res), "{at}: {res:?}'s bar at {x}");
            assert!(
                order(b) >= last,
                "{at}: {res:?}'s bar names {b:?} at {x}, out of order"
            );
            last = order(b);
            match b {
                Bar::Tab => named[0] = true,
                Bar::Aterm => named[1] = true,
                Bar::Global => {}
            }
        }
    }
    named
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
    let mut named = [0_u32; 2];
    let mut tally = |n: [bool; 2]| {
        for (seen, n) in named.iter_mut().zip(n) {
            *seen += u32::from(n);
        }
    };
    for (w, s) in [(1119.0, 2.0), (900.0, 2.0), (550.0, 2.0), (350.0, 1.0)] {
        let p = plan::plan_full(w, &m);
        let at = format!("Full {:?} at {w} pt, {s}×", p.kind);
        let laid = laid(Scene::Calm, w, s, DeckForm::Full, &m, true);
        let hits = &laid.hits;
        for (i, &res) in DECK_RESOURCES.iter().enumerate() {
            let cols: Vec<Col> = [p.name, strip_col(p.strip)]
                .into_iter()
                .chain(p.pct)
                .chain(p.meter.run)
                .chain(p.details)
                .collect();
            tally(check_row(
                hits,
                s,
                (res, p.rows, i as u32),
                &cols,
                p.meter.bar,
                &at,
            ));
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
            for (res, r) in DECK_RESOURCES
                .iter()
                .copied()
                .skip(bi * 4)
                .take(4)
                .zip(0_u32..)
            {
                let cols: Vec<Col> = [b.name, strip_col(b.strip)]
                    .into_iter()
                    .chain(b.pct)
                    .chain(b.meter.run)
                    .chain(b.details)
                    .collect();
                tally(check_row(
                    hits,
                    s,
                    (res, b.rows, r),
                    &cols,
                    b.meter.bar,
                    &at,
                ));
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
        // The gauge at the top left is the Cores hit, as Full's slot is, over the header line too.
        let g = p.gauge.expect("CONTROL: the gauge at every width here");
        let (cx, cy) = g.centre();
        for y in [6.0, cy] {
            assert_eq!(
                hit_at(hits, cx, y, s),
                Some(HitKind::Cores),
                "{at}: the gauge"
            );
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
    assert!(
        named.iter().all(|&n| n > 3),
        "CONTROL: rows whose bar named the tab's and aterm's segments: {named:?}"
    );
    let w = folded_width(BlockKind::Slim, &m);
    let laid = laid(Scene::Trouble, w, 2.0, DeckForm::Folded, &m, true);
    let head = laid.head.as_ref().expect("a header");
    let labels: Vec<_> = head
        .labels
        .iter()
        .filter(|l| matches!(l.label, Label::State(_)))
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
                    .is_some_and(|st| says_state(&label.text, &st.label))
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
            (700.0, 2.0),
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
    assert_eq!(written.len(), scenes.len() * 6 * 2 * 2);
}

#[test]
fn time_gaps_and_special_event_lines_render_distinctly_on_the_multi_scale_strip() {
    let m = FaceMetrics::helvetica_neue();
    let mut snap = scene(Scene::Calm);
    snap.time_gaps = vec![
        aterm_vitals::TimeGap {
            start_ms: AT - 500_000,
            end_ms: AT - 320_000,
            kind: aterm_vitals::GapKind::Sleep,
        },
        aterm_vitals::TimeGap {
            start_ms: AT - 150_000,
            end_ms: AT - 80_000,
            kind: aterm_vitals::GapKind::Crash,
        },
    ];
    snap.strip_events = vec![
        aterm_vitals::StripEvent {
            at_unix_ms: AT - 150_000,
            kind: aterm_vitals::StripEventKind::Crash,
            label: "aterm crash".into(),
        },
        aterm_vitals::StripEvent {
            at_unix_ms: AT - 80_000,
            kind: aterm_vitals::StripEventKind::Restart,
            label: "aterm restart".into(),
        },
        aterm_vitals::StripEvent {
            at_unix_ms: AT - 320_000,
            kind: aterm_vitals::StripEventKind::SleepWake,
            label: "wake".into(),
        },
        aterm_vitals::StripEvent {
            at_unix_ms: AT - 25_000,
            kind: aterm_vitals::StripEventKind::PressureSpike,
            label: "memory pressure".into(),
        },
    ];
    let inks = super::tests::dark_inks();
    for form in [DeckForm::Full, DeckForm::Folded] {
        for contrast in [false, true] {
            let w = 1119.0;
            let s = 2.0;
            let mut input = scene_input(&snap, form);
            input.contrast = contrast;
            let words = DeckWords::new(&input, &m);
            let laid = lay(&geom(w, s, form), &input, &words, &m, true);
            let strip = match form {
                DeckForm::Full => plan::plan_full(w, &m).strip,
                DeckForm::Folded => plan::plan_folded(w, &m).blocks[0].strip,
            };

            // Gap cells emit horizontal `DrawPrim::Dashed` top/bottom rails (`h == 0.0 && w > 0.0`),
            // distinct from hollow_cell (`DrawPrim::Stroke` in `inks.hollow`).
            let gap_rails = laid
                .prims
                .iter()
                .filter(|&p| matches!(*p, DrawPrim::Dashed { w, h, .. } if w > 0.0 && h == 0.0))
                .count();
            assert!(
                gap_rails >= 4,
                "{form:?}: sleep/crash time gaps emit horizontal dashed gap rails ({gap_rails})"
            );

            // Verify each of the 4 event kinds renders at its exact continuous X coordinate on the
            // multi-scale strip (`strip.x_at_age_ms`).
            for ev in &snap.strip_events {
                let x_pt = strip
                    .x_at_age_ms(AT - ev.at_unix_ms)
                    .expect("event within 15m/1h horizon");
                let ex = (x_pt * s) as f32;
                let want_rgb = match ev.kind {
                    aterm_vitals::StripEventKind::Crash => inks.critical_text,
                    aterm_vitals::StripEventKind::Restart => inks.warn_text,
                    aterm_vitals::StripEventKind::SleepWake => inks.stuck,
                    aterm_vitals::StripEventKind::PressureSpike => inks.warn_text,
                };
                // Cap pip on the X-axis rail.
                let has_pip = laid.prims.iter().any(|p| match *p {
                    DrawPrim::Panel { x, w, h, fill, .. } => {
                        (x + w / 2.0 - ex).abs() <= 2.0
                            && (w - h).abs() < 0.01
                            && w >= 3.0
                            && [fill[0], fill[1], fill[2]] == want_rgb
                    }
                    _ => false,
                });
                assert!(
                    has_pip,
                    "{form:?}: {:?} missing X-axis cap pip at {ex}",
                    ev.kind
                );

                // Vertical event line through the strip rows: solid Panel for Crash/Restart, vertical
                // Dashed for SleepWake/PressureSpike.
                let has_line = match ev.kind {
                    aterm_vitals::StripEventKind::Crash | aterm_vitals::StripEventKind::Restart => {
                        laid.prims.iter().any(|p| match *p {
                            DrawPrim::Panel { x, w, h, fill, .. } => {
                                w <= 3.0
                                    && h > 15.0
                                    && (x + w / 2.0 - ex).abs() <= 2.0
                                    && [fill[0], fill[1], fill[2]] == want_rgb
                            }
                            _ => false,
                        })
                    }
                    aterm_vitals::StripEventKind::SleepWake
                    | aterm_vitals::StripEventKind::PressureSpike => {
                        laid.prims.iter().any(|p| match *p {
                            DrawPrim::Dashed { x, w, h, color, .. } => {
                                w == 0.0
                                    && h > 15.0
                                    && (x - ex).abs() <= 2.0
                                    && [color[0], color[1], color[2]] == want_rgb
                                    && if contrast {
                                        color[3] >= 0xD0
                                    } else {
                                        color[3] > 0 && color[3] < 0x80
                                    }
                            }
                            _ => false,
                        })
                    }
                };
                assert!(
                    has_line,
                    "{form:?}: {:?} missing vertical event marker line at {ex}",
                    ev.kind
                );
            }
        }
    }
}

/// Every scope bar nests the three scopes, 0.144.0's bar as main draws it (the owner, 2026-10-26:
/// "the bar of "this Mac, aterm, Tab 1" that's great"): on the five resources the machine's fill,
/// aterm's inset in it and this tab's inset in that, from one left edge. Where a run stands at
/// its end it says each row's reading with its maximum (the legend then names no totals), and
/// the network's rates both ways; where none does (Folded at 1424 pt, whose strips keep the room)
/// the legend names this Mac with its totals and no row says a figure past its strip. Full's
/// Details say aterm's disk writes and the network's state and round trip; Folded's Cores row
/// names the chip, and a hot core paints in the heat ramp's ink with no halo.
#[test]
fn scope_bars_nest_every_scope_and_the_run_says_each_rows_maximum() {
    let m = FaceMetrics::helvetica_neue();
    let mut snap = scene(Scene::Calm);
    snap.facts.cpu_brand = Some("Apple M5 Max".into());
    snap.net = Some(aterm_vitals::NetView {
        tcp: Some(aterm_vitals::TcpCounts {
            established: 24,
            ..Default::default()
        }),
        state: aterm_vitals::NetState::Online,
        rates: Some(aterm_vitals::LinkRates {
            rx_bps: 12_400_000.0,
            tx_bps: 1_800_000.0,
            ..Default::default()
        }),
        srtt_ms: Some(14.0),
        aterm_sockets: Some(8),
        tab_sockets: vec![(FRONT, 3)],
        ..Default::default()
    });
    if let Some(a) = snap.aterm.as_mut() {
        a.read_bps = Some(8_000_000.0);
        a.write_bps = Some(4_000_000.0);
    }
    if let Some(t) = snap.tabs.first_mut() {
        t.scope.read_bps = Some(3_000_000.0);
    }
    if let Some(cpu) = snap.cpu.as_mut() {
        if let Some(c0) = cpu.cores.get_mut(0) {
            c0.load.level = 0.15;
        }
        if let Some(c1) = cpu.cores.get_mut(1) {
            c1.load.level = 0.95;
        }
    }

    let inks = super::tests::dark_inks();
    for (form, w, run) in [
        (DeckForm::Full, 1424.0, true),
        (DeckForm::Folded, 2560.0, true),
        (DeckForm::Folded, 1424.0, false),
    ] {
        let s = 2.0;
        let input = scene_input(&snap, form);
        let words = DeckWords::new(&input, &m);
        for res in [
            Resource::Cpu,
            Resource::Memory,
            Resource::Disk,
            Resource::Network,
            Resource::Processes,
        ] {
            let row = words.rows.iter().find(|r| r.res == res).expect("row");
            let fill = row
                .bar
                .unwrap_or_else(|| panic!("{form:?} {res:?}: missing BarFill"));
            assert!(
                fill.global.is_some_and(|v| v > 0.0)
                    && fill.aterm.is_some_and(|v| v > 0.0)
                    && fill.tab.is_some_and(|v| v > 0.0),
                "{form:?} {res:?}: expected all 3 scopes positive, got {fill:?}"
            );
        }

        let laid = lay(&geom(w, s, form), &input, &words, &m, true);
        let texts: Vec<&str> = laid
            .boxes
            .iter()
            .filter(|b| b.kind == BoxKind::Text)
            .map(|b| b.text.as_str())
            .collect();
        let at = format!("{form:?} {w} pt");
        let totals = "this Mac \u{b7} 18 cores \u{b7} 128 GiB \u{b7} 2 TB";
        let legend = if run { "this Mac" } else { totals };
        assert_eq!(
            texts.iter().filter(|t| **t == legend).count(),
            1,
            "{at}: the legend: {texts:?}"
        );
        for said in ["of 18 cores", "\u{2193}12.4M \u{2191}1.8M/s"] {
            assert_eq!(
                texts.iter().any(|t| t.contains(said)),
                run,
                "{at}: {said:?} in {texts:?}"
            );
        }
        if run {
            for said in ["128 GiB used", "2.00 TB used"] {
                assert!(
                    texts.iter().any(|t| t.contains(said)),
                    "{at}: physical used/total reading {said:?} in {texts:?}"
                );
            }
        }
        // aterm's disk writes, which lost their column, are in Full's Details.
        if form == DeckForm::Full {
            for said in ["aterm writes", "Online", "14 ms"] {
                assert!(
                    texts.iter().any(|t| t.contains(said)),
                    "{at}: {said:?} in Details: {texts:?}"
                );
            }
        }
        assert_eq!(
            texts.iter().any(|t| t.contains("M5 Max")),
            form == DeckForm::Folded,
            "{at}: Folded's Cores row names the chip, Full's dial none: {texts:?}"
        );

        // As painted: all five resources keep three visible nested scopes, including
        // capacity rows that move their bar under the percentage.
        let [machine_c, aterm_c, tab_c] =
            [inks.machine, inks.aterm, inks.tab].map(|c| rgba(c, 0xFF));
        let mut nested = 0;
        for (ti, (rows, pct, bar, under)) in bar_tables(form, w, &m).into_iter().enumerate() {
            let first = if form == DeckForm::Full { 0 } else { ti * 4 };
            for r in 0..rows.count {
                let Some(&res) = DECK_RESOURCES.get(first + r as usize) else {
                    continue;
                };
                if res == Resource::Gpu {
                    continue;
                }
                let moved = capacity_under(form, w, ti, res, &m);
                let horizontal = moved || bar.is_none();
                let col =
                    if moved { pct } else { bar.or(under) }.expect("CONTROL: scoped resource bar");
                let band = if horizontal {
                    (
                        px(rows.top(r) + rows.baseline + plan::UNDER_GAP, s),
                        px(rows.top(r + 1), s),
                    )
                } else {
                    (px(rows.top(r), s), px(rows.top(r + 1), s))
                };
                let panels = panels_at(&laid.prims, px(col.x, s), band);
                let scope = |ink| {
                    let matches: Vec<Rect> = panels
                        .iter()
                        .filter_map(|&(rect, fill)| (fill == ink).then_some(rect))
                        .collect();
                    assert_eq!(matches.len(), 1, "{at} {res:?}: one {ink:?} scope");
                    matches[0]
                };
                let (gx, gy, gw, gh) = scope(machine_c);
                let (ax, ay, aw, ah) = scope(aterm_c);
                let (tx, ty, tw, th) = scope(tab_c);
                assert!(
                    [gx, gy, gw, gh, ax, ay, aw, ah, tx, ty, tw, th]
                        .iter()
                        .all(|v| v.fract() == 0.0),
                    "{at} {res:?}: all scope edges fall on whole device pixels"
                );
                assert_eq!((gx, ax), (tx, tx), "{at} {res:?}: shared left edge");
                if horizontal {
                    assert_eq!(
                        (gy, gh, ay, ah),
                        (ty, th, ty, th),
                        "{at} {res:?}: scopes share the line"
                    );
                    assert!(
                        gw >= aw + 1.0 && aw >= tw + 1.0 && tw >= 1.0 && gh >= 1.0,
                        "{at} {res:?}: each scope retains visible ink: {gw} >= {aw} >= {tw}"
                    );
                } else {
                    assert!(
                        gw >= aw
                            && aw >= tw
                            && tw > 0.0
                            && gh > ah
                            && ah > th
                            && ay > gy
                            && ay + ah < gy + gh
                            && ty > ay
                            && ty + th < ay + ah,
                        "{at} {res:?}: nested bar geometry {gw}x{gh} >= {aw}x{ah} >= {tw}x{th}"
                    );
                }
                nested += 1;
            }
        }
        assert_eq!(
            nested, 5,
            "{form:?}: CPU, memory, disk, network and processes nest their three scopes"
        );
    }

    // A hot core (0.95) paints in the heat ramp's ink like every data cell, with no halo.
    let folded_input = scene_input(&snap, DeckForm::Folded);
    let folded_words = DeckWords::new(&folded_input, &m);
    let folded_laid = lay(
        &geom(1424.0, 2.0, DeckForm::Folded),
        &folded_input,
        &folded_words,
        &m,
        true,
    );
    let hot_core_fill = rgba(inks.heat[usize::from(ink::heat_step(0.95))], 0xFF);
    let hot_core = folded_laid.prims.iter().find_map(|p| match *p {
        DrawPrim::Panel {
            x, y, w, h, fill, ..
        } if (w - h).abs() < 0.01 && fill == hot_core_fill => Some((x, y, w)),
        _ => None,
    });
    let (hx, hy, _) = hot_core.expect("hot 0.95 core square painted in the heat ramp's ink");
    assert!(
        !folded_laid.prims.iter().any(|p| matches!(
            *p,
            DrawPrim::Stroke { x, y, color, .. }
                if (x - (hx - 0.5)).abs() < 0.01 && (y - (hy - 0.5)).abs() < 0.01 && color[3] < 0xFF
        )),
        "a hot core square draws no halo"
    );

    // The header names no hardware: Folded's Cores row names the chip once at every tier that
    // has room for it (mid at 1074 pt, narrow at 750 pt).
    for w in [1074.0, 750.0] {
        let laid = lay(
            &geom(w, 2.0, DeckForm::Folded),
            &folded_input,
            &folded_words,
            &m,
            true,
        );
        let chip = laid
            .boxes
            .iter()
            .filter(|b| b.kind == BoxKind::Text && b.text.contains("M5 Max"))
            .count();
        assert_eq!(chip, 1, "Folded {w} pt: the Cores row names the chip once");
    }
}

/// No column of figures stands after the time series (the owner, 2026-10-26: "the stats in
/// columns after the time series doesn't make sense to me"): right of each row's strip and short
/// of its Details, every word stands in the row's one figure run, attached to the end of its bar,
/// and says the reading then at most a state, from the run column's left edge; a table with no
/// run says nothing there. CONTROL: runs are read, and a figure laid in a column of its own fails
/// the check.
#[test]
fn no_column_of_figures_stands_after_the_strip() {
    let m = FaceMetrics::helvetica_neue();
    for sc in [Scene::Calm, Scene::Trouble, Scene::FirstRun, Scene::Worst] {
        let mut runs_by_form = [0, 0];
        for (form, w) in [
            (DeckForm::Full, 1039.0),
            (DeckForm::Full, 700.0),
            (DeckForm::Full, 450.0),
            (DeckForm::Folded, 2560.0),
            (DeckForm::Folded, 1424.0),
            (DeckForm::Folded, 1039.0),
            (DeckForm::Folded, 940.0),
        ] {
            let laid = laid(sc, w, 2.0, form, &m, true);
            let tables: Vec<(Strip, Rows, plan::Meter, Option<Col>, f64)> = match form {
                DeckForm::Full => {
                    let p = plan::plan_full(w, &m);
                    vec![(p.strip, p.rows, p.meter, p.details, w)]
                }
                DeckForm::Folded => {
                    let p = plan::plan_folded(w, &m);
                    p.blocks
                        .iter()
                        .map(|b| (b.strip, b.rows, b.meter, b.details, b.at.end()))
                        .collect()
                }
            };
            assert!(
                !tables.is_empty(),
                "CONTROL: {sc:?} {form:?} {w} pt has tables"
            );
            for (strip, rows, meter, details, end) in tables {
                let at = format!("{sc:?} {form:?} {w} pt");
                // A table with no run: an empty one at its bar's end, which no word stands in.
                let run = meter.run.unwrap_or(Col {
                    x: meter.bar.map_or(strip.end(), Col::end),
                    w: 0.0,
                });
                let faults = column_faults(&laid.boxes, (strip, rows, run, details, end));
                assert!(faults.is_empty(), "{at}: {faults:?}");
                runs_by_form[usize::from(form == DeckForm::Folded)] += laid
                    .boxes
                    .iter()
                    .filter(|b| b.kind == BoxKind::Text && b.room == Some((run.x, run.end())))
                    .count();
                // CONTROL: a figure in a column of its own, past the run's end, is found.
                let mut stray = laid.boxes.clone();
                let r = rows.top(0) + rows.baseline;
                stray.push(Boxed {
                    kind: BoxKind::Text,
                    r: [run.end() - 20.0, r - 7.0, run.end() - 2.0, r + 2.0],
                    text: "3.1".into(),
                    room: Some((run.end() - 20.0, run.end())),
                });
                let faults = column_faults(&stray, (strip, rows, run, details, end));
                assert!(
                    !faults.is_empty(),
                    "CONTROL: {at}: a column of figures passed"
                );
            }
        }
        assert!(
            runs_by_form.iter().all(|&n| n > 0),
            "CONTROL: {sc:?} reads actual runs in both forms: {runs_by_form:?}"
        );
    }
}

/// What a table `(strip, rows, run, details, end)` of `boxes` says right of its strip and short of
/// its Details (or `end`) outside its figure run, or in it but not from its left edge.
fn column_faults(
    boxes: &[Boxed],
    (strip, rows, run, details, end): (Strip, Rows, Col, Option<Col>, f64),
) -> Vec<String> {
    let stop = details.map_or(end, |d| d.x);
    let mut faults = Vec::new();
    for r in 0..rows.count.min(7) {
        let (y0, y1) = (rows.top(r) - 1.0, rows.top(r) + rows.cell + 1.0);
        let mut said: Vec<&Boxed> = boxes
            .iter()
            .filter(|b| {
                let y = (b.r[1] + b.r[3]) / 2.0;
                b.kind == BoxKind::Text && b.r[0] > strip.end() && b.r[0] < stop && y > y0 && y < y1
            })
            .collect();
        said.sort_by(|a, b| a.r[0].total_cmp(&b.r[0]));
        for (i, b) in said.iter().enumerate() {
            if b.room != Some((run.x, run.end())) {
                faults.push(format!(
                    "row {r}: {:?} at {} outside the run {run:?}",
                    b.text, b.r[0]
                ));
            } else if i == 0
                && (b.r[0] - run.x).abs() > 0.01
                && (b.r[0] - run.x - 1.1 * rows.amount_pt).abs() > 0.01
            {
                faults.push(format!(
                    "row {r}: {:?} starts at {}, not at the run's {}",
                    b.text, b.r[0], run.x
                ));
            }
        }
        if said.len() > 2 {
            faults.push(format!("row {r}: {} words past the strip", said.len()));
        }
    }
    faults
}

#[path = "stats_deck_owner_scenes.rs"]
mod owner_scenes;
