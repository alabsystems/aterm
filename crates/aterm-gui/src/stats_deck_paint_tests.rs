// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scene and gallery tests for the stats deck heat-table painter.

use super::*;
use crate::stats_paint::HitKind;
use crate::stats_paint::tests::{job_row, zombies};
use crate::tray_raster::stroke_px;
use aterm_vitals::explain::WhyFacts;
use aterm_vitals::explain::fixtures::{self, spotlight_1032};
use aterm_vitals::jobs::{Findings, JobsView, Verdict};
use aterm_vitals::model::{
    AccountIx, Busiest, CapBasis, ClusterView, CoreView, CpuView, DiskFacts, GpuSample, GpuView,
    Hog, LinkKind, LinkSample, MemSample, MemView, NoFigure, PlanFact, Pressure, ProcScope,
    ScopeFig, Series, TabView, TaskRoom, Thermal, Throttle, VolumeSample,
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
                over: cell.over_n > 0.0,
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
            let n = if v.is_some() { 10.0 } else { 0.0 };
            Cell {
                mean: v,
                max: v,
                n,
                over_n: if o { n } else { 0.0 },
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
    f.room = TaskRoom {
        procs: Some(if worst { 999_999 } else { 897 }),
        procs_max: Some(if worst { 9_999_999 } else { 16_000 }),
        user_procs: Some(if worst { 999_999 } else { 578 }),
        user_procs_max: Some(if worst { 4_194_304 } else { 10_666 }),
        ..TaskRoom::default()
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
    utc_offset_at: None,
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
        network_place: None,
        network_target: crate::network_deck::Target::Overview,
        lab: None,
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

/// Where [`sweep`] stops laying every point at 1×: above it no word a deck draws changes (the last
/// change is at 3171 pt in any job), a premise [`sweep`] holds at the 64ths it lays there.
const EVERY_POINT_TO: i32 = 3200;

/// The widths [`sweep`] lays above [`EVERY_POINT_TO`], at either scale.
fn widths_above() -> impl Iterator<Item = i32> {
    (EVERY_POINT_TO + 64..=5120).step_by(64)
}

/// The widths [`sweep`] lays at 2×: every seventh point, odd and even widths alike, each width
/// where the words change at 1× (`words_change`), and [`widths_above`].
fn widths_at_2x(words_change: &[i32]) -> Vec<i32> {
    let mut widths: Vec<i32> = (160..=EVERY_POINT_TO)
        .step_by(7)
        .chain(words_change.iter().copied())
        .chain(widths_above())
        .collect();
    widths.sort_unstable();
    widths.dedup();
    widths
}

/// What a layout draws in words, in any order: each box's kind and text, hashed and summed.
fn drawn_words(laid: &Laid) -> u64 {
    use std::hash::{Hash, Hasher};
    laid.boxes
        .iter()
        .map(|b| {
            let mut h = std::hash::DefaultHasher::new();
            (b.kind as u8).hash(&mut h);
            b.text.hash(&mut h);
            h.finish()
        })
        .fold(0, u64::wrapping_add)
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

/// What each check found over the T-G4 sweep: one pass lays every scene out at the widths
/// [`sweep`] says, and each test reads its own check's findings (the key and the cells on the
/// worst scene only).
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

/// One pass of the T-G4 sweep over every scene, face and form. At 1× it lays every whole point
/// from 160 pt to [`EVERY_POINT_TO`]; at 2× it lays every seventh point, each width where the words
/// drawn at 1× change ([`drawn_words`]), and at both, every 64th point above that to 5120 pt. The
/// stride of seven reaches odd and even widths alike: `lay_folded` floors a halved width, so odd
/// widths have their own slack.
///
/// Why those stand for every width. The plans floor to whole points and draw the same words at
/// both scales at a width, so 1× sees every plan a point at a time and those lays draw every word
/// layout at 2× too, a two-point island among them; what 2× adds is device-pixel rounding, whose
/// faults run for hundreds of widths. Above [`EVERY_POINT_TO`] the words do not change; the
/// positions of what is not a bar still do, every 2 pt, from the same arithmetic 1× sweeps below
/// it. Both premises are held here, as [`Check::Whole`] findings: 2× draws 1×'s words at each width
/// it lays, and no 64th above [`EVERY_POINT_TO`] draws other words than that width. What is not
/// implied, and would be missed: a 2×-only fault at a width none of these lays and narrower than
/// seven points where the words do not change, and any fault between the 64ths above
/// [`EVERY_POINT_TO`], a change of words included.
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
            for form in [DeckForm::Full, DeckForm::Folded] {
                jobs.push((*sc, snap, *face, m, form));
            }
        }
    }
    let check = &check;
    std::thread::scope(|scope| {
        let running: Vec<_> = jobs
            .into_iter()
            .map(|(sc, snap, face, m, form)| {
                scope.spawn(move || {
                    let input = scene_input(snap, form);
                    let words = DeckWords::new(&input, m);
                    let mut failed: Vec<(Check, String)> = Vec::new();
                    let note = |failed: &mut Vec<(Check, String)>,
                                c: Check,
                                s: f64,
                                w: f64,
                                why: String| {
                        if failed.iter().filter(|(f, _)| *f == c).count() <= 20 {
                            failed.push((
                                c,
                                format!("{sc:?}, {face}, {s}×, {form:?}, {w} pt: {why}"),
                            ));
                        }
                    };
                    let laid_at = |failed: &mut Vec<(Check, String)>, s: f64, w: i32| {
                        let w = f64::from(w);
                        let laid = lay(&geom(w, s, form), &input, &words, m, false);
                        let drawn = drawn_words(&laid);
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
                            note(failed, c, s, w, why);
                        }
                        drawn
                    };
                    let mut at_1x = std::collections::BTreeMap::new();
                    let mut words_change = Vec::new();
                    let mut last = None;
                    for w in (160..=EVERY_POINT_TO).chain(widths_above()) {
                        let drawn = laid_at(&mut failed, 1.0, w);
                        if last != Some(drawn) {
                            words_change.push(w);
                        }
                        last = Some(drawn);
                        at_1x.insert(w, drawn);
                    }
                    for (&w, drawn) in at_1x.range(EVERY_POINT_TO + 1..) {
                        if Some(drawn) != at_1x.get(&EVERY_POINT_TO) {
                            let why = format!("other words than at {EVERY_POINT_TO} pt");
                            note(&mut failed, Check::Whole, 1.0, f64::from(w), why);
                        }
                    }
                    for w in widths_at_2x(&words_change) {
                        let drawn = laid_at(&mut failed, 2.0, w);
                        if at_1x.get(&w) != Some(&drawn) {
                            let why = "other words than at 1×".to_string();
                            note(&mut failed, Check::Whole, 2.0, f64::from(w), why);
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
            if first + r as usize >= DECK_RESOURCES.len() {
                continue;
            }
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

/// Every row that prints a % draws its scope bar, in every form, on both faces at 1× and 2×: in
/// its column right of the strip where it has one, else under the figure, inside the row's pitch
/// (`docs/DESIGN-stats-deck-meter-2026-10-26.md` §2), the GPU's and Memory's in the bar column
/// like every other row's (`docs/DESIGN-stats-deck-flow-2026-11-02.md` §3). The widths are
/// [`sweep`]'s: every point at 1× and every seventh at 2× to [`EVERY_POINT_TO`], with 2× also at
/// each width where the words change, and above it every 64th point, the only stride at which
/// positions are guarded there. CONTROL: a row that prints no % (an unread GPU; the record's
/// Latency row is not drawn yet) draws no bar.
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
        let col = bar.or(bar_under).expect("CONTROL: the table's bar column");
        // Under-percent bars note the whole device-pixel rect they actually draw.
        let bars_in = |laid: &Laid, r: u32| {
            laid.boxes
                .iter()
                .filter(|b| {
                    b.kind == BoxKind::Bar
                        && (b.r[0] - col.x).abs() <= 0.25
                        && b.r[1] >= rows.top(r)
                        && b.r[1] < rows.top(r + 1)
                })
                .count()
        };
        for (r, &res) in DECK_RESOURCES.iter().take(rows.count as usize).enumerate() {
            let bars = bars_in(&laid, r as u32);
            assert_eq!(
                bars, 1,
                "CONTROL {form:?} {w} pt {res:?}: its bar at {col:?}"
            );
        }
        let gpu = DECK_RESOURCES
            .iter()
            .position(|&x| x == Resource::Gpu)
            .expect("a GPU row") as u32;
        let mut unread = scene(Scene::Calm);
        unread.resources[Resource::Gpu.index()] = ResourceView::default();
        unread.gpu = None;
        if let Some(gpu) = unread.facts.gpu.as_mut() {
            gpu.device_pct = None;
        }
        let input = scene_input(&unread, form);
        let words = DeckWords::new(&input, &m);
        let none = lay(&geom(w, 2.0, form), &input, &words, &m, false);
        // Its row's words, in this table (Folded's block B stands on the same line).
        let said: Vec<&str> = none
            .boxes
            .iter()
            .filter(|b| {
                b.kind == BoxKind::Text
                    && b.r[0] < col.end()
                    && b.r[3] > rows.top(gpu)
                    && b.r[3] <= rows.top(gpu + 1)
            })
            .map(|b| b.text.as_str())
            .collect();
        assert!(
            said.contains(&"GPU") && !said.iter().any(|t| t.ends_with('%')),
            "CONTROL {form:?} {w} pt: an unread GPU prints no %: {said:?}"
        );
        assert_eq!(bars_in(&none, gpu), 0, "{form:?} {w} pt: no % and no bar");
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
/// least a device px where the fill around it has one to spare; the column bar's own inks in both themes and under Increase Contrast; and
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
                        let (wide_rows, _, wide_bar, wide_under) = wide_tables[ti];
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
                            // before it (a fill clamps to the one around it, A1 §4.6).
                            assert!(
                                fills.windows(2).skip(1).all(|p| p[1].0 <= p[0].0),
                                "{at}: {fills:?}"
                            );
                            nested += usize::from(fills.len() == 4);
                            // The column's inks: the same, but where a fill around one is a
                            // px, the line's scope inside it gives way (A1 §4.6), the column's
                            // rim showing it still.
                            let wide_col = wide_bar
                                .or(wide_under)
                                .expect("CONTROL: the wide resource's bar");
                            let band = if wide_bar.is_none() {
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
                            let theirs: Vec<[u8; 4]> =
                                panels_at(&column.prims, px(wide_col.x, s), band)
                                    .into_iter()
                                    .map(|(_, fill)| fill)
                                    .filter(|f| bar_inks.contains(f))
                                    .collect();
                            let ours: Vec<[u8; 4]> = fills.iter().map(|f| f.1).collect();
                            assert!(
                                ours.iter().all(|f| theirs.contains(f)),
                                "{at}: {ours:?} are the column's inks {theirs:?}"
                            );
                            if ours.len() < theirs.len() {
                                assert!(
                                    fills.iter().skip(1).any(|f| f.0 <= 1.0),
                                    "{at}: a scope gave way inside no 1 px fill: {fills:?}"
                                );
                            }
                            // A click anywhere on it, its foot past the row's band included,
                            // opens the row's lens; past it the next row's band takes over.
                            let mid = x + tw / 2.0;
                            for py in [y + 0.5, y + h - 0.5] {
                                let hit =
                                    hit_at(&narrow.hits, f64::from(mid) / s, f64::from(py) / s, s);
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

/// The now dot's beat, widest, meets no words and no mark at the widths [`sweep`] lays, in either
/// form or face, at 1× or 2× (the % column ends `PCT_TO_STRIP` before the strip, past the ring's
/// reach).
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
    let details_in = |laid: &Laid, at: (Col, f64, f64)| -> (Vec<String>, Vec<Boxed>) {
        let (col, top, cell) = at;
        let texts = texts_in(laid, col, top - 1.0, top + cell + 1.0);
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
        (texts, marks)
    };
    let drawn = |sc: Scene, w: f64, form: DeckForm| -> Option<(Vec<String>, Vec<Boxed>)> {
        let at = cpu_details(w, form, &m)?;
        Some(details_in(&laid(sc, w, 2.0, form, &m, false), at))
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

    // The Details fold by their column's width alone: each width the plans give the CPU's
    // Details column is laid once, at the narrowest deck giving it, on words read once per form.
    let snaps = [Scene::Calm, Scene::Trouble].map(scene);
    let mut seen = 0;
    for form in [DeckForm::Full, DeckForm::Folded] {
        let inputs = snaps.each_ref().map(|snap| scene_input(snap, form));
        let words = inputs.each_ref().map(|input| DeckWords::new(input, &m));
        let mut widths: Vec<u64> = Vec::new();
        for w in (160..=5120).map(f64::from) {
            let Some(details) = cpu_details(w, form, &m) else {
                continue;
            };
            if widths.contains(&details.0.w.to_bits()) {
                continue;
            }
            widths.push(details.0.w.to_bits());
            let [(calm, calm_marks), (texts, marks)] = [0, 1].map(|i| {
                let laid = lay(&geom(w, 2.0, form), &inputs[i], &words[i], &m, false);
                details_in(&laid, details)
            });
            assert!(
                calm.iter().all(|t| !t.starts_with("Spotlight"))
                    && calm_marks.iter().all(|b| b.text != "cause"),
                "Calm holds no cause, {form:?} {w} pt: {calm:?}"
            );
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

/// No two core squares touch, at 1x and 2x, in both forms, on 18 cores and on 128. A Cores row's
/// squares come from its table's rows and Cores mode alone (the rows' cell; the mode's side and
/// pitch, on a lattice whose origin is a whole device px), so the first and last width of each
/// (rows, mode) the plans give from 300 to 2600 pt stand for every width of it.
#[test]
fn core_squares_never_touch() {
    let m = FaceMetrics::helvetica_neue();
    let mut drew: Vec<(DeckForm, CoresMode)> = Vec::new();
    for sc in [Scene::Calm, Scene::Worst] {
        let snap = scene(sc);
        for form in [DeckForm::Full, DeckForm::Folded] {
            let input = scene_input(&snap, form);
            let words = DeckWords::new(&input, &m);
            let clusters = words.clusters();
            // (ground_only, the Cores row's table rows, its mode) -> its first and last width.
            type Class = (bool, Rows, CoresMode);
            let mut classes: Vec<(Class, (f64, f64))> = Vec::new();
            for w in (300..=2600).map(f64::from) {
                let class = match form {
                    DeckForm::Full => {
                        let p = plan::plan_full(w, &m);
                        let mode = p.cores_mode(&clusters, &m).unwrap_or(CoresMode::CountOnly);
                        (p.ground_only, p.rows, mode)
                    }
                    DeckForm::Folded => {
                        let p = plan::plan_folded(w, &m);
                        let b = p.blocks[1];
                        (p.ground_only, b.rows, b.cores_mode(&clusters, &m))
                    }
                };
                match classes.iter_mut().find(|(c, _)| *c == class) {
                    Some((_, (_, last))) => *last = w,
                    None => classes.push((class, (w, w))),
                }
            }
            for s in [1.0, 2.0] {
                for &((_, _, mode), (first, last)) in &classes {
                    for w in [first, last] {
                        let laid = lay(&geom(w, s, form), &input, &words, &m, true);
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
                            if !inside.is_empty() && !drew.contains(&(form, mode)) {
                                drew.push((form, mode));
                            }
                        }
                    }
                }
            }
        }
    }
    let modes: Vec<CoresMode> = drew.iter().map(|d| d.1).collect();
    for want in [CoresMode::Big, CoresMode::Stack, CoresMode::Tiny] {
        assert!(
            modes.contains(&want),
            "CONTROL: no {want:?} squares drawn: {drew:?}"
        );
    }
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
                    // The outline inks the box's own edge px, the px its plate covers read.
                    let outline = laid.prims.iter().any(|p| {
                        matches!(*p, DrawPrim::Stroke { x: sx, y: sy, w: sw, h: sh, radius, color, width }
                            if color == hollow && width == 1.0
                                && stroke_px((sx, sy, sw, sh), radius, width) == (x, y, cw, h))
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

/// The strip presets with the widths each lays from, pt ([`Strip::spec_for`]).
const PRESET_RANGES: [(MultiScaleSpec, f64, f64); 4] = [
    (MultiScaleSpec::FULL_6H, 239.0, 1600.0),
    (MultiScaleSpec::FOLD_4H, 129.0, 238.0),
    (MultiScaleSpec::COMPACT_6M, 79.0, 128.0),
    (MultiScaleSpec::MINI_1M, 59.0, 78.0),
];

/// `form` at `w` pt draws only its ground.
fn ground_only(w: f64, form: DeckForm, m: &FaceMetrics) -> bool {
    match form {
        DeckForm::Full => plan::plan_full(w, m).ground_only,
        DeckForm::Folded => plan::plan_folded(w, m).ground_only,
    }
}

/// A strip `w` pt wide from `x`, its preset the one its width takes.
fn strip_of(x: f64, w: f64) -> Strip {
    Strip {
        x,
        cols: 1,
        period_s: 10,
        pitch: w + 1.0,
        cell: w,
        spec: Strip::spec_for(w),
    }
}

/// Every row's history area on `w` pt of `form` at `s`, device px `(x0, y0, x1, y1)`: the strip's
/// span, from `above` px over the row's top (its over caps) to its foot.
fn strip_areas(
    w: f64,
    s: f64,
    form: DeckForm,
    m: &FaceMetrics,
    above: f32,
) -> Vec<(f32, f32, f32, f32)> {
    strips(w, form, m)
        .into_iter()
        .flat_map(|(strip, rows, at)| {
            at.into_iter().map(move |(r, _)| {
                let y = px(rows.top(r), s);
                (
                    px(strip.x, s),
                    y - above,
                    px(strip.end(), s),
                    y + px(rows.cell, s),
                )
            })
        })
        .collect()
}

/// The gates (`docs/DESIGN-stats-deck-units-2026-11-04.md` §3): on each preset, at the first width
/// each form lays it, at 1× and 2×, two boxes of one band stand 1 pt apart (1 device px at 1×, 2
/// at 2×) and two bands 3 pt (3 px at 1×, 6 at 2×), whatever [`BOX_GAP`] and [`plan::GATE`]
/// say; nothing but the card's ground draws in a gate down any row's history; and no column in
/// the grid ink stands on a strip. The record's gridlines
/// (`docs/DESIGN-stats-deck-owner-2026-10-26.md` §4) are retired: the gate's ground tells the
/// bands apart.
#[test]
fn every_gate_is_a_three_point_ground_gap_and_no_gridline_is_drawn() {
    let m = FaceMetrics::helvetica_neue();
    let snap = scene(Scene::Calm);
    assert!(
        snap.time_gaps.is_empty() && snap.strip_events.is_empty(),
        "CONTROL: no gap mark or event line crosses a gate"
    );
    let inks = super::tests::dark_inks();
    let (ground, grid) = (rgba(inks.ground, 0xFF), rgba(inks.grid, 0xFF));
    assert_ne!(grid, ground, "CONTROL: a gridline would show on the ground");
    let presets = PRESET_RANGES.map(|(spec, ..)| spec);
    let (mut failed, mut gates, mut laid_presets) = (Vec::new(), [0_usize; 4], [false; 4]);
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
                    let at_str = format!("{form:?} {w} pt at {s}×, strip {bi}");
                    let rects = multi_scale_slot_rects(&strip, s, 0.0);
                    let slots = strip.spec.slots();
                    let (x0, x1) = (px(strip.x, s), px(strip.end(), s));
                    let ys: Vec<(f32, f32)> = at
                        .iter()
                        .map(|&(r, _)| {
                            let y = px(rows.top(r), s);
                            (y, y + px(rows.cell, s))
                        })
                        .collect();
                    for pair in rects.windows(2) {
                        let ((i, ax, aw, _), (j, bx, ..)) = (pair[0], pair[1]);
                        let a_end = ax + aw;
                        let gate = slots[i].band != slots[j].band;
                        // The rule's own widths, pt, not the constants that lay them.
                        let gap_pt: f64 = if gate { 3.0 } else { 1.0 };
                        let want = (gap_pt * s) as f32;
                        if bx - a_end != want {
                            failed.push(format!(
                                "{at_str}: slots {i} and {j} stand {} px apart, not {want}",
                                bx - a_end
                            ));
                        }
                        if !gate {
                            continue;
                        }
                        gates[pi] += 1;
                        for &(y0, y1) in &ys {
                            for p in &laid.prims {
                                let (qx, qy, qw, qh, fill) = match *p {
                                    DrawPrim::Panel {
                                        x, y, w, h, fill, ..
                                    } => (x, y, w, h, fill),
                                    DrawPrim::Stroke {
                                        x,
                                        y,
                                        w,
                                        h,
                                        radius,
                                        width,
                                        color,
                                    } => {
                                        let (x, y, w, h) = stroke_px((x, y, w, h), radius, width);
                                        (x, y, w, h, color)
                                    }
                                    DrawPrim::Dashed {
                                        x,
                                        y,
                                        w,
                                        h,
                                        width,
                                        color,
                                        ..
                                    } => (x, y, w.max(width), h.max(width), color),
                                    _ => continue,
                                };
                                if fill != ground
                                    && qx < bx
                                    && qx + qw > a_end
                                    && qy < y1
                                    && qy + qh > y0
                                {
                                    failed.push(format!(
                                        "{at_str}: {p:?} draws in the gate {a_end}..{bx} px"
                                    ));
                                }
                            }
                            // The box after the gate: its plate, or an unread box's outline.
                            let boxed = laid.prims.iter().any(|p| match *p {
                                DrawPrim::Panel { x, y, fill, .. } => {
                                    x == bx && y == y0 && fill != ground
                                }
                                DrawPrim::Stroke {
                                    x,
                                    y,
                                    w,
                                    h,
                                    radius,
                                    width,
                                    ..
                                } => {
                                    let (sx, sy, _, _) = stroke_px((x, y, w, h), radius, width);
                                    sx == bx && sy == y0
                                }
                                _ => false,
                            });
                            if !boxed {
                                failed.push(format!(
                                    "CONTROL {at_str}: no box at {bx} px, after the gate"
                                ));
                            }
                        }
                    }
                    for p in &laid.prims {
                        if let DrawPrim::Panel {
                            x, y, w, h, fill, ..
                        } = *p
                            && fill == grid
                            && (x0..x1).contains(&x)
                            && ys.iter().any(|&(y0, y1)| y < y1 && y + h > y0)
                        {
                            failed.push(format!(
                                "{at_str}: a {w} px column in the grid ink at {x} px"
                            ));
                        }
                    }
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
        gates.iter().all(|&g| g > 0),
        "CONTROL: every preset's gates were read ({gates:?})"
    );
    assert!(
        failed.is_empty(),
        "{} faults over {gates:?} gates (6 h, 4 h, 6 min, 1 min):\n{}",
        failed.len(),
        failed.join("\n")
    );
}

/// The header's ticks and the boxes come from one geometry ([`Strip::box_edges`]): at every
/// width a plan lays, at 1× and 2×, every band's first age ([`MultiScaleSpec::ticks`]) puts its
/// tick ([`tick_px`] of [`Strip::x_at_age_ms`]) inside the gate before that band's first box, a
/// pixel at most off its middle; and every tick a laid header draws stands in a gate or at the
/// strip's old end. CONTROL: an age inside a box puts its x inside that box, not a gate.
#[test]
fn header_ticks_and_box_rects_come_from_one_geometry() {
    let m = FaceMetrics::helvetica_neue();
    let (mut failed, mut ticks) = (Vec::new(), 0_usize);
    for form in [DeckForm::Full, DeckForm::Folded] {
        for w in sweep_widths().step_by(3) {
            if ground_only(w, form, &m) {
                continue;
            }
            for (strip, ..) in strips(w, form, &m) {
                let slots = strip.spec.slots();
                for s in [1.0, 2.0] {
                    let rects = multi_scale_slot_rects(&strip, s, 0.0);
                    let at_str = format!("{form:?} {w} pt at {s}×, {} pt strip", strip.width());
                    for &(age_ms, _) in strip.spec.ticks {
                        let Some(j) = slots.iter().position(|sl| sl.age_hi_ms == age_ms) else {
                            continue;
                        };
                        if j + 1 >= rects.len() {
                            continue;
                        }
                        let Some(x) = strip.x_at_age_ms(age_ms) else {
                            failed.push(format!("{at_str}: no x for {age_ms} ms"));
                            continue;
                        };
                        let (tx, tw) = tick_px(x, s);
                        let (a, b) = (rects[j].1 + rects[j].2, rects[j + 1].1);
                        ticks += 1;
                        if !(tx >= a && tx + tw <= b && ((tx - a) - (b - tx - tw)).abs() <= 1.0) {
                            failed.push(format!(
                                "{at_str}: the {age_ms} ms tick at {tx}+{tw} px, its gate {a}..{b}"
                            ));
                        }
                        let (_, bx, bw, _) = rects[j + 1];
                        let lo = slots[j + 1].age_lo_ms;
                        let mid = lo + (slots[j + 1].age_hi_ms - lo) / 2;
                        let inside = strip.x_at_age_ms(mid).map(|x| (x * s) as f32);
                        assert!(
                            bw < 4.0 || inside.is_some_and(|ix| ix > bx && ix < bx + bw),
                            "CONTROL {at_str}: {mid} ms is inside its box {bx}+{bw}: {inside:?}"
                        );
                    }
                }
            }
        }
    }
    let snap = scene(Scene::Calm);
    for form in [DeckForm::Full, DeckForm::Folded] {
        for (w, s) in [(1119.0, 2.0), (1119.0, 1.0), (560.0, 1.0), (300.0, 2.0)] {
            let input = scene_input(&snap, form);
            let words = DeckWords::new(&input, &m);
            let laid = lay(&geom(w, s, form), &input, &words, &m, true);
            let Some(head) = laid.head.as_ref() else {
                continue;
            };
            let Some((strip, ..)) = strips(w, form, &m).into_iter().next() else {
                continue;
            };
            let rects = multi_scale_slot_rects(&strip, s, 0.0);
            let gates: Vec<(f32, f32)> = rects
                .windows(2)
                .filter(|p| strip.spec.slots()[p[0].0].band != strip.spec.slots()[p[1].0].band)
                .map(|p| (p[0].1 + p[0].2, p[1].1))
                .collect();
            for tick in &head.ruler {
                let (tx, tw) = tick_px(tick.x, s);
                ticks += 1;
                if !gates.iter().any(|&(a, b)| tx >= a && tx + tw <= b) {
                    failed.push(format!(
                        "{form:?} {w} pt at {s}×: the header's {} tick at {tx} px stands in no gate \
                         {gates:?}",
                        tick.text
                    ));
                }
            }
            if let Some(x) = head.time_tick {
                assert!(
                    (x - strip.end()).abs() < 1e-9,
                    "{form:?} {w} pt: the time tick at the strip's old end"
                );
            }
        }
    }
    assert!(ticks > 1000, "CONTROL: ticks were read ({ticks})");
    assert!(
        failed.is_empty(),
        "{} faults over {ticks} ticks:\n{}",
        failed.len(),
        failed.join("\n")
    );
}

/// The rising weights hold at device px (`docs/DESIGN-stats-deck-units-2026-11-04.md` §3): from
/// the one-second band up, no box is narrower than a box of a finer band, at every quarter point
/// of each preset's range (FULL_6H to 1600 pt), from eight offsets an eighth of a point apart, at
/// 1× and 2×, and on every strip a plan lays. Edges on a half pixel are where `f32` fractions
/// rounded both ways ([`Strip::box_edges`]).
#[test]
fn from_one_second_up_no_box_is_narrower_than_a_box_of_a_finer_band() {
    let m = FaceMetrics::helvetica_neue();
    let mut failed = Vec::new();
    let mut checked = 0_usize;
    let mut check = |strip: Strip, s: f64, failed: &mut Vec<String>| {
        let slots = strip.spec.slots();
        let mut bands: Vec<(f32, f32)> = vec![(f32::INFINITY, 0.0); strip.spec.bands.len()];
        for (i, _, w, live) in multi_scale_slot_rects(&strip, s, 0.0) {
            if live {
                continue;
            }
            let b = &mut bands[usize::from(slots[i].band)];
            *b = (b.0.min(w), b.1.max(w));
        }
        checked += 1;
        let read: Vec<(usize, (f32, f32))> = bands
            .into_iter()
            .enumerate()
            .filter(|(_, (lo, _))| lo.is_finite())
            .collect();
        for (k, &(fine, (_, fine_max))) in read.iter().enumerate() {
            for &(coarse, (coarse_min, _)) in &read[k + 1..] {
                if coarse_min < fine_max {
                    failed.push(format!(
                        "{} pt from {} at {s}×: band {coarse}'s {coarse_min} px box under band \
                         {fine}'s {fine_max} px",
                        strip.width(),
                        strip.x
                    ));
                }
            }
        }
    };
    for (spec, lo, hi) in PRESET_RANGES {
        let mut w = lo;
        while w <= hi {
            for x in (0..8).map(|k| f64::from(k) / 8.0) {
                let strip = strip_of(x, w);
                assert_eq!(strip.spec, spec, "CONTROL: {w} pt lays its preset");
                for s in [1.0, 2.0] {
                    check(strip, s, &mut failed);
                }
            }
            w += 0.25;
        }
    }
    for form in [DeckForm::Full, DeckForm::Folded] {
        for w in sweep_widths() {
            for (strip, ..) in strips(w, form, &m) {
                for s in [1.0, 2.0] {
                    check(strip, s, &mut failed);
                }
            }
        }
    }
    assert!(checked > 10_000, "CONTROL: strips were read ({checked})");
    failed.dedup();
    assert!(
        failed.is_empty(),
        "{} faults:\n{}",
        failed.len(),
        failed[..failed.len().min(40)].join("\n")
    );
}

/// A box's column is its mean and its tick its largest reading, on the deck as painted: on every
/// strip of Full and Folded at 1× and 2×, each read box's column stands on its foot `mean · H`
/// px tall (at least one), in the heat of its mean; its tick one px tall at `max · H`, in the heat
/// of its max, where that clears the column by two px, and none where it does not.
#[test]
fn a_box_column_is_its_mean_and_its_tick_its_max() {
    let m = FaceMetrics::helvetica_neue();
    let snap = scene(Scene::Calm);
    let (mut columns, mut ticks, mut heights) =
        (0_usize, 0_usize, std::collections::BTreeSet::new());
    for (form, w, s) in [
        (DeckForm::Full, 1119.0, 2.0),
        (DeckForm::Full, 1119.0, 1.0),
        (DeckForm::Full, 350.0, 1.0),
        (DeckForm::Folded, 1119.0, 2.0),
        (DeckForm::Folded, 550.0, 1.0),
    ] {
        let input = scene_input(&snap, form);
        let inks = input.inks;
        let ic = input.contrast;
        let words = DeckWords::new(&input, &m);
        let laid = lay(&geom(w, s, form), &input, &words, &m, true);
        let heat_inks: Vec<[u8; 4]> = inks
            .heat
            .iter()
            .chain(&inks.heat_ic)
            .map(|&c| rgba(c, 0xFF))
            .collect();
        for (strip, rows, at) in strips(w, form, &m) {
            let rects = multi_scale_slot_rects(&strip, s, 0.0);
            for (r, res) in at {
                let row = words
                    .rows
                    .iter()
                    .find(|row| row.res == res)
                    .expect("its row");
                if row.strip_text.is_some() {
                    continue;
                }
                let slots = row.multi_scale_slots(&strip);
                let (y, h) = (px(rows.top(r), s), px(rows.cell, s));
                for &(i, x, cw, live) in &rects {
                    let Some(slot) = slots.get(i).filter(|_| !live) else {
                        continue;
                    };
                    let Some(mean) = slot.cell.heat.filter(|_| slot.cell.gap.is_none()) else {
                        continue;
                    };
                    let tag = format!("{form:?} {w} pt at {s}×, {res:?} slot {i}");
                    let found: Vec<(f32, f32, [u8; 4])> = laid
                        .prims
                        .iter()
                        .filter_map(|p| match *p {
                            DrawPrim::Panel {
                                x: qx,
                                y: qy,
                                w: qw,
                                h: qh,
                                fill,
                                ..
                            } if qx >= x
                                && qx + qw <= x + cw
                                && qy >= y
                                && qy + qh <= y + h
                                && heat_inks.contains(&fill) =>
                            {
                                Some((qy, qh, fill))
                            }
                            _ => None,
                        })
                        .collect();
                    let want = (mean.max(0.0) * h).round().clamp(1.0, h);
                    let column = found.iter().find(|f| f.0 + f.1 == y + h);
                    assert_eq!(
                        column.map(|c| (c.1, c.2)),
                        Some((want, rgba(inks.cell_at(mean, ic), 0xFF))),
                        "{tag}: the column at its mean {mean}"
                    );
                    columns += 1;
                    heights.insert(want as u32);
                    let tick = found.iter().find(|f| f.1 == 1.0 && f.0 + 1.0 < y + h);
                    let th = slot
                        .cell
                        .max
                        .filter(|v| v.is_finite())
                        .map(|v| ((v.max(0.0) * h).round().clamp(1.0, h), v));
                    match th {
                        Some((th, v)) if th >= want + 2.0 => {
                            assert_eq!(
                                tick.map(|t| (t.0, t.2)),
                                Some((y + h - th, rgba(inks.cell_at(v, ic), 0xFF))),
                                "{tag}: the tick at its max {v}"
                            );
                            ticks += 1;
                        }
                        _ => assert!(
                            tick.is_none(),
                            "{tag}: a tick that does not clear: {tick:?}"
                        ),
                    }
                }
            }
        }
    }
    assert!(
        columns > 100 && ticks > 0 && heights.len() >= 3,
        "CONTROL: {columns} columns of {heights:?} px and {ticks} ticks were read"
    );
}

/// The over cap stands outside its box's plate, 1 device px clear of the plate's top and 2 px tall
/// at 2×, flush and 1 px tall at 1× (A1 §3; the rule's px, not [`skyline::cap_px`]'s), and keeps
/// at least a device px of ground under the row above (v2 defect 8: a 1× cap touched or entered
/// it). Row 0's caps stand under the header's axis rail and ticks, which end on one foot: a
/// device px of ground between at 2×, flush at 1× (A1 §3 as built: the rail cannot rise into the
/// header words' descenders). On every read box of every strip, every box over, of Full and
/// Folded at five widths, at 1× and 2×.
#[test]
fn the_over_cap_sits_outside_the_plate_and_clear_of_the_row_above() {
    let m = FaceMetrics::helvetica_neue();
    let snap = scene(Scene::Calm);
    let mut caps = [0_usize; 2];
    for form in [DeckForm::Full, DeckForm::Folded] {
        for w in [1119.0, 700.0, 420.0, 250.0, 180.0] {
            if ground_only(w, form, &m) {
                continue;
            }
            for s in [1.0, 2.0] {
                let input = scene_input(&snap, form);
                let over = rgba(input.inks.over, 0xFF);
                let mut words = DeckWords::new(&input, &m);
                for row in &mut words.rows {
                    row.live_over = 1.0;
                    for slots in &mut row.multi_scale {
                        for slot in slots {
                            slot.cell.over = 1.0;
                        }
                    }
                }
                let laid = lay(&geom(w, s, form), &input, &words, &m, true);
                let (clear, cap_h): (f32, f32) = if s > 1.0 { (1.0, 2.0) } else { (0.0, 1.0) };
                let grid = rgba(input.inks.grid, 0xFF);
                for (strip, rows, at) in strips(w, form, &m) {
                    let rects = multi_scale_slot_rects(&strip, s, 0.0);
                    let (sx0, sx1) = (px(strip.x, s), px(strip.end(), s));
                    // The axis rail's foot, where the ticks end too.
                    let rail = laid.prims.iter().find_map(|p| match *p {
                        DrawPrim::Panel {
                            x, y, w, h, fill, ..
                        } if fill == grid && h == 1.0 && x == sx0 && x + w == sx1 => Some(y + 1.0),
                        _ => None,
                    });
                    let rail = rail.unwrap_or_else(|| {
                        panic!("CONTROL {form:?} {w} pt at {s}×: the strip's axis rail")
                    });
                    for &(r, res) in &at {
                        let row = words
                            .rows
                            .iter()
                            .find(|row| row.res == res)
                            .expect("its row");
                        if row.strip_text.is_some() {
                            continue;
                        }
                        let slots = row.multi_scale_slots(&strip);
                        let (y, _) = (px(rows.top(r), s), px(rows.cell, s));
                        // The foot of what stands above and the ground the cap keeps under it.
                        let above = match r.checked_sub(1) {
                            Some(ra) => (px(rows.top(ra), s) + px(rows.cell, s), 1.0),
                            None => (rail, if s > 1.0 { 1.0 } else { 0.0 }),
                        };
                        for &(i, x, cw, live) in &rects {
                            let slot = slots.get(i).copied().unwrap_or_default();
                            let read = slot.cell.gap.is_none()
                                && if live {
                                    row.heat_now.or(slot.cell.heat).is_some()
                                } else {
                                    slot.cell.heat.is_some()
                                };
                            let tag = format!("{form:?} {w} pt at {s}×, {res:?} slot {i}");
                            let found: Vec<(f32, f32)> = laid
                                .prims
                                .iter()
                                .filter_map(|p| match *p {
                                    DrawPrim::Panel {
                                        x: qx,
                                        y: qy,
                                        w: qw,
                                        h: qh,
                                        fill,
                                        ..
                                    } if fill == over
                                        && qx >= x
                                        && qx + qw <= x + cw
                                        && qy + qh <= y
                                        && qy >= y - clear - cap_h - 2.0 =>
                                    {
                                        Some((qy, qh))
                                    }
                                    _ => None,
                                })
                                .collect();
                            assert_eq!(found.len(), usize::from(read), "{tag}: {found:?}");
                            let Some(&(cy, ch)) = found.first() else {
                                continue;
                            };
                            assert_eq!(ch, cap_h, "{tag}: its height");
                            assert_eq!(cy + ch, y - clear, "{tag}: outside the plate");
                            let (b, keep) = above;
                            assert!(
                                cy >= b + keep,
                                "{tag}: the cap at {cy} px keeps {} px under what stands above, \
                                 its foot at {b}, not {keep}",
                                cy - b
                            );
                            caps[usize::from(s > 1.0)] += 1;
                        }
                    }
                }
            }
        }
    }
    assert!(
        caps.iter().all(|&n| n > 100),
        "CONTROL: caps were read ({caps:?})"
    );
}

/// The live box (A1 §3, "Live box"; `ResourceView::live_box`), as the deck paints it: its tick
/// is the last 10 s's largest reading, one device px tall at `round(peak·H)` from its foot where
/// that clears the 10 s mean's column by 2 px, not its slot's own max (CONTROL: the slot's max
/// stands elsewhere and draws nothing); a stale deck mixes the tick toward the ground by 0.55,
/// as it does the column (CONTROL: a fresh deck draws both in their heat); and a deck whose only
/// over reading is in a live box's 10 s keys "over max" (CONTROL: with that reading not over, no
/// key). Row 0's live box, Full and Folded at 1119 pt, at 1× and 2×.
#[test]
fn the_live_box_ticks_its_ten_second_peak_dims_it_stale_and_keys_its_over() {
    let m = FaceMetrics::helvetica_neue();
    let snap = scene(Scene::Calm);
    let (mean, peak, slot_max) = (0.3_f32, 0.7_f32, 0.95_f32);
    let w = 1119.0;
    let mut ticks = 0;
    for form in [DeckForm::Full, DeckForm::Folded] {
        for s in [1.0, 2.0] {
            let tag = format!("{form:?} {w} pt at {s}×");
            let (strip, rows, at) = strips(w, form, &m).swap_remove(0);
            let res = at[0].1;
            let (_, x, cw, _) = multi_scale_slot_rects(&strip, s, 0.0)
                .into_iter()
                .find(|r| r.3)
                .expect("the live box");
            let (y, h) = (px(rows.top(0), s), px(rows.cell, s));
            let rise = |v: f32| (v * h).round().clamp(1.0, h);
            assert!(
                rise(peak) >= rise(mean) + 2.0 && rise(slot_max) != rise(peak),
                "CONTROL {tag}: the peak clears the column and the slot's max stands apart"
            );
            let inset = if cw >= 5.0 { 1.0 } else { 0.0 };
            let tick_at = |prims: &[DrawPrim], v: f32| -> Vec<[u8; 4]> {
                prims
                    .iter()
                    .filter_map(|p| match *p {
                        DrawPrim::Panel {
                            x: qx,
                            y: qy,
                            w: qw,
                            h: qh,
                            fill,
                            ..
                        } if qh == 1.0
                            && qx == x + inset
                            && qw == cw - 2.0 * inset
                            && qy == y + h - rise(v) =>
                        {
                            Some(fill)
                        }
                        _ => None,
                    })
                    .collect()
            };
            let column_at = |prims: &[DrawPrim]| -> Vec<[u8; 4]> {
                let ch = rise(mean);
                prims
                    .iter()
                    .filter_map(|p| match *p {
                        DrawPrim::Panel {
                            x: qx,
                            y: qy,
                            w: qw,
                            h: qh,
                            fill,
                            ..
                        } if qx == x && qw == cw && qy == y + h - ch && qh == ch => Some(fill),
                        _ => None,
                    })
                    .collect()
            };
            let paint = |live: crate::stats_live::DeckLive, over: f32| {
                let mut input = scene_input(&snap, form);
                input.live = live;
                let mut words = DeckWords::new(&input, &m);
                for row in &mut words.rows {
                    row.live_over = 0.0;
                    for slots in &mut row.multi_scale {
                        for slot in slots {
                            slot.cell.over = 0.0;
                            if slot.slot.is_live_now {
                                slot.cell.max = Some(slot_max);
                            }
                        }
                    }
                    if row.res == res {
                        row.heat_now = Some(mean);
                        row.live_peak = Some(peak);
                        row.live_over = over;
                    }
                }
                lay(&geom(w, s, form), &input, &words, &m, true)
            };
            let inks = super::tests::dark_inks();
            let (hot, warm) = (
                inks.heat[usize::from(ink::heat_step(peak))],
                inks.heat[usize::from(ink::heat_step(mean))],
            );
            let fresh = paint(crate::stats_live::DeckLive::STILL, 0.0);
            assert_eq!(
                tick_at(&fresh.prims, peak),
                [rgba(hot, 0xFF)],
                "{tag}: the live box's tick at its 10 s peak, in its heat"
            );
            assert!(
                tick_at(&fresh.prims, slot_max).is_empty(),
                "CONTROL {tag}: nothing at the slot's own max"
            );
            assert_eq!(column_at(&fresh.prims), [rgba(warm, 0xFF)], "CONTROL {tag}");
            let stale = paint(crate::stats_live::DeckLive::STALE, 0.0);
            assert_eq!(
                tick_at(&stale.prims, peak),
                [rgba(ink::mix(inks.ground, hot, 0.55), 0xFF)],
                "{tag}: a stale live box dims its tick"
            );
            assert_eq!(
                column_at(&stale.prims),
                [rgba(ink::mix(inks.ground, warm, 0.55), 0xFF)],
                "{tag}: as it dims its column"
            );
            assert_ne!(hot, inks.ground, "CONTROL: the dim moves the ink");
            ticks += 1;
            let keyed = |laid: &Laid| {
                laid.boxes
                    .iter()
                    .any(|b| b.kind == BoxKind::Text && b.text == "over max")
            };
            assert!(
                keyed(&paint(crate::stats_live::DeckLive::STILL, 0.4)),
                "{tag}: the live box's 10 s alone is over, and the key says over max"
            );
            assert!(!keyed(&fresh), "CONTROL {tag}: nothing over, no over max");
        }
    }
    assert_eq!(ticks, 4, "CONTROL: both forms at both scales");
}

/// A sleep is `hollow`, nothing read, never `stuck`, the colour of a stuck process
/// (`docs/DESIGN-stats-deck-units-2026-11-04.md` §3, "States"): its gap's boxes fill toward
/// `hollow` with `hollow` rails, its wake's line is `hollow`, and nothing on the strips draws
/// in `stuck`; each event line keeps a device px of ground each side. CONTROL: a crash's gap
/// draws in its own ink beside it, so the kinds are told apart.
#[test]
fn a_sleep_gap_is_hollow_not_stuck() {
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
            at_unix_ms: AT - 320_000,
            kind: aterm_vitals::StripEventKind::SleepWake,
            label: "wake".into(),
        },
        aterm_vitals::StripEvent {
            at_unix_ms: AT - 150_000,
            kind: aterm_vitals::StripEventKind::Crash,
            label: "aterm crash".into(),
        },
    ];
    let inks = super::tests::dark_inks();
    assert_ne!(inks.stuck, inks.hollow, "CONTROL: the two inks differ");
    let rgb = |c: [u8; 4]| [c[0], c[1], c[2]];
    let sleep_fill = ink::mix(inks.ground, inks.hollow, 0.14);
    let stuck_fill = ink::mix(inks.ground, inks.stuck, 0.14);
    let crash_fill = ink::mix(inks.ground, inks.critical_text, 0.14);
    let ground = rgba(inks.ground, 0xFF);
    for form in [DeckForm::Full, DeckForm::Folded] {
        let (w, s) = (1119.0, 2.0);
        let input = scene_input(&snap, form);
        let words = DeckWords::new(&input, &m);
        let laid = lay(&geom(w, s, form), &input, &words, &m, true);
        let fills = |want: Rgb| {
            laid.prims
                .iter()
                .filter(|p| matches!(**p, DrawPrim::Panel { fill, .. } if rgb(fill) == want))
                .count()
        };
        assert!(
            fills(sleep_fill) > 0,
            "{form:?}: the sleep's boxes fill toward hollow"
        );
        assert!(
            fills(crash_fill) > 0,
            "CONTROL {form:?}: the crash's boxes keep their ink"
        );
        let areas = strip_areas(w, s, form, &m, (3.0 * s) as f32);
        let on_strip = |x: f32, y: f32| {
            areas
                .iter()
                .any(|a| x >= a.0 && x < a.2 && y >= a.1 && y < a.3)
        };
        let stuck = laid.prims.iter().find(|p| match **p {
            DrawPrim::Panel { x, y, fill, .. } => {
                on_strip(x, y) && (rgb(fill) == inks.stuck || rgb(fill) == stuck_fill)
            }
            DrawPrim::Dashed { x, y, color, .. } | DrawPrim::Stroke { x, y, color, .. } => {
                on_strip(x, y) && rgb(color) == inks.stuck
            }
            _ => false,
        });
        assert!(stuck.is_none(), "{form:?}: stuck ink on a strip: {stuck:?}");
        let rails = laid
            .prims
            .iter()
            .filter(|p| {
                matches!(**p, DrawPrim::Dashed { w, h, color, .. }
                    if w > 0.0 && h == 0.0 && rgb(color) == inks.hollow)
            })
            .count();
        assert!(rails > 0, "{form:?}: the sleep's rails are hollow");
        let strip = match form {
            DeckForm::Full => plan::plan_full(w, &m).strip,
            DeckForm::Folded => plan::plan_folded(w, &m).blocks[0].strip,
        };
        for (age, want) in [(320_000, inks.hollow), (150_000, inks.critical_text)] {
            let ex = (strip.x_at_age_ms(age).expect("on the strip") * s) as f32;
            let lines: Vec<(f32, f32, f32, f32)> = laid
                .prims
                .iter()
                .filter_map(|p| match *p {
                    DrawPrim::Dashed {
                        x,
                        y,
                        w,
                        h,
                        width,
                        color,
                        ..
                    } if w == 0.0 && h > 15.0 && (x - ex).abs() <= 2.0 && rgb(color) == want => {
                        Some((x, y, width, h))
                    }
                    DrawPrim::Panel {
                        x, y, w, h, fill, ..
                    } if w <= 3.0
                        && h > 15.0
                        && (x + w / 2.0 - ex).abs() <= 2.0
                        && rgb(fill) == want =>
                    {
                        Some((x, y, w, h))
                    }
                    _ => None,
                })
                .collect();
            assert!(
                !lines.is_empty(),
                "{form:?}: the {age} ms event's line in its ink"
            );
            for (lx, ly, lw, lh) in lines {
                for kx in [lx - 1.0, lx + lw] {
                    let keyline = laid.prims.iter().any(|p| {
                        matches!(*p, DrawPrim::Panel { x, y, w, h, fill, .. }
                            if x == kx && y == ly && w == 1.0 && h == lh && fill == ground)
                    });
                    assert!(
                        keyline,
                        "{form:?}: a ground keyline at {kx} px by the {age} ms line"
                    );
                }
            }
        }
    }
}

/// The three gap kinds paint apart (`docs/DESIGN-stats-deck-owner-2026-10-26.md` §6): one gap over
/// the same span, drawn as a crash, a sleep and a pause, paints three different frames with
/// Increase Contrast off and on, both forms at 1× and 2×. Increase Contrast lifts sleep to its
/// `hollow` (`words2`) and paused from `words2` to `words`, so the two never meet in one ink
/// (A1 §3 as built). CONTROL: a kind painted twice paints the same frame, and a gap paints a
/// frame the deck without it does not.
#[test]
fn the_three_gap_kinds_paint_apart_with_increase_contrast_off_and_on() {
    use aterm_vitals::GapKind;
    let m = FaceMetrics::helvetica_neue();
    let base = scene(Scene::Calm);
    let w = 1119.0;
    let frame = |kind: Option<GapKind>, form: DeckForm, s: f64, contrast: bool| {
        let mut snap = base.clone();
        snap.time_gaps = kind
            .map(|kind| aterm_vitals::TimeGap {
                start_ms: AT - 500_000,
                end_ms: AT - 320_000,
                kind,
            })
            .into_iter()
            .collect();
        let mut input = scene_input(&snap, form);
        input.contrast = contrast;
        let words = DeckWords::new(&input, &m);
        format!(
            "{:?}",
            lay(&geom(w, s, form), &input, &words, &m, true).prims
        )
    };
    let kinds = [GapKind::Crash, GapKind::Sleep, GapKind::Paused];
    for form in [DeckForm::Full, DeckForm::Folded] {
        for s in [1.0, 2.0] {
            for contrast in [false, true] {
                let tag = format!("{form:?} {w} pt at {s}×, contrast {contrast}");
                let frames = kinds.map(|k| frame(Some(k), form, s, contrast));
                for (i, a) in frames.iter().enumerate() {
                    for (j, b) in frames.iter().enumerate().skip(i + 1) {
                        assert_ne!(
                            a, b,
                            "{tag}: a {:?} gap paints as a {:?} one",
                            kinds[i], kinds[j]
                        );
                    }
                }
                assert_eq!(
                    frames[1],
                    frame(Some(GapKind::Sleep), form, s, contrast),
                    "CONTROL {tag}: a frame paints the same twice"
                );
                assert_ne!(
                    frames[1],
                    frame(None, form, s, contrast),
                    "CONTROL {tag}: the gap paints"
                );
            }
        }
    }
}

/// The lowest heat step stands apart from the plate it draws on: in every built-in theme, and
/// the deck's own dark and light inks, `heat[0]` and `heat_ic[0]` are 0.04 or more of OkLab
/// lightness from `track` (`docs/DESIGN-stats-deck-units-2026-11-04.md` §3), so a read zero's
/// one-pixel foot shows on its plate.
#[test]
fn the_lowest_heat_step_stands_apart_from_the_plate_in_every_theme() {
    let dl =
        |a: Rgb, b: Rgb| (aterm_vitals::ink::to_lab(a).l - aterm_vitals::ink::to_lab(b).l).abs();
    let probe = super::tests::dark_inks();
    assert!(
        dl(probe.track, probe.track) == 0.0 && dl(probe.ground, probe.track) >= 0.04,
        "CONTROL: the measure tells the plate from the ground"
    );
    let names = aterm_types::scheme::builtin_names();
    assert!(
        names.len() > 10,
        "CONTROL: the built-in themes ({})",
        names.len()
    );
    let mut all: Vec<(String, VitalsInks)> = names
        .iter()
        .map(|name| ((*name).to_owned(), theme_inks(name)))
        .collect();
    all.push(("deck dark".into(), probe));
    all.push(("deck light".into(), light_inks()));
    let mut faults = Vec::new();
    for (name, inks) in &all {
        for (ramp, step0) in [("heat", inks.heat[0]), ("heat_ic", inks.heat_ic[0])] {
            let d = dl(step0, inks.track);
            if d < 0.04 {
                faults.push(format!(
                    "{name}: {ramp}[0] {step0:?} is {d:.3} from track {:?}",
                    inks.track
                ));
            }
        }
    }
    assert!(
        faults.is_empty(),
        "{} of {} themes:\n{}",
        faults.len(),
        all.len(),
        faults.join("\n")
    );
}

/// A strip reads by height alone: the strips draw the same boxes with Differentiate Without
/// Color on as off (the strip's DWC branch is retired: every box's value is already its height),
/// and with one ink for the whole heat ramp; and those heights carry the reading.
#[test]
fn a_strip_reads_by_height_alone_without_colour() {
    let m = FaceMetrics::helvetica_neue();
    let snap = scene(Scene::Calm);
    for (form, w, s) in [
        (DeckForm::Full, 1119.0, 2.0),
        (DeckForm::Folded, 1119.0, 1.0),
    ] {
        let areas = strip_areas(w, s, form, &m, (3.0 * s) as f32);
        let boxes = |input: &DeckInput<'_>| -> Vec<(f32, f32, f32, f32)> {
            let words = DeckWords::new(input, &m);
            let laid = lay(&geom(w, s, form), input, &words, &m, true);
            laid.prims
                .iter()
                .filter_map(|p| match *p {
                    DrawPrim::Panel { x, y, w, h, .. }
                        if areas
                            .iter()
                            .any(|a| x >= a.0 && x + w <= a.2 && y >= a.1 && y + h <= a.3) =>
                    {
                        Some((x, y, w, h))
                    }
                    _ => None,
                })
                .collect()
        };
        let plain = scene_input(&snap, form);
        let mut dwc = scene_input(&snap, form);
        dwc.dwc = true;
        let mut mono = scene_input(&snap, form);
        let one = mono.inks.words2;
        mono.inks.heat = [one; 7];
        mono.inks.heat_ic = [one; 7];
        let drawn = boxes(&plain);
        assert_eq!(
            drawn,
            boxes(&dwc),
            "{form:?}: DWC draws the strips as they are"
        );
        assert_eq!(
            drawn,
            boxes(&mono),
            "{form:?}: one ink draws the same boxes"
        );
        let heights: std::collections::BTreeSet<u32> = drawn.iter().map(|b| b.3 as u32).collect();
        assert!(
            drawn.len() > 100 && heights.len() >= 4,
            "CONTROL {form:?}: {} boxes of {heights:?} px",
            drawn.len()
        );
    }
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
    let forms = words::glance_run(res, &snap.resources[res.index()], snap, &MAC);
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

/// The calm scene with the machine at the OS's thermal level `raw` (exact), the CPU row's state
/// as the model derives it from that level.
fn thermal_scene(raw: u8) -> Snapshot {
    let level = [
        Throttle::None,
        Throttle::Light,
        Throttle::Heavy,
        Throttle::Severe,
    ][usize::from(raw)];
    let thermal = Thermal {
        level,
        raw,
        low_power: Some(false),
        exact: true,
    };
    let mut snap = scene(Scene::Calm);
    snap.thermal = Some(thermal);
    snap.resources[Resource::Cpu.index()].state = thermal.state();
    snap
}

/// Where row `res` of `snap` says its state at every Folded and Full kind, as
/// [`state_is_said_in_words_without_details`] reads it: its well's mark, then the first of its
/// Details, else its figure run, else its block's header. Answers each mark drawn in its well
/// and each header's state label, by kind.
fn state_said(snap: &Snapshot, res: Resource, m: &FaceMetrics) -> Vec<(String, Vec<String>)> {
    let st = words::state(res, snap, &MAC);
    let i = DECK_RESOURCES
        .iter()
        .position(|&r| r == res)
        .expect("a deck row");
    let lay_at = |w: f64, form: DeckForm| {
        let input = scene_input(snap, form);
        let words = DeckWords::new(&input, m);
        lay(&geom(w, 2.0, form), &input, &words, m, false)
    };
    let marks = |laid: &Laid, well: Col, rows: Rows, r: u32| -> Vec<String> {
        let (top, bottom) = (rows.top(r), rows.top(r) + rows.cell);
        laid.boxes
            .iter()
            .filter(|b| {
                b.kind == BoxKind::Mark
                    && b.r[0] >= well.x - EPS
                    && b.r[2] <= well.end() + EPS
                    && (b.r[1] + b.r[3]) / 2.0 > top
                    && (b.r[1] + b.r[3]) / 2.0 < bottom
            })
            .map(|b| b.text.clone())
            .collect()
    };
    let labels = |laid: &Laid, block: u8| -> Vec<String> {
        laid.head
            .iter()
            .flat_map(|h| &h.labels)
            .filter(|l| l.label == Label::State(block))
            .map(|l| l.text.clone())
            .collect()
    };
    let mut out = Vec::new();
    let said = |laid: &Laid,
                (details, meter, rows, r): (Option<Col>, plan::Meter, Rows, u32),
                block: u8,
                ours: &[Resource],
                at: &str| {
        let Some(st) = st.as_ref() else {
            return;
        };
        match (details, meter.run) {
            (Some(details), _) => {
                let top = rows.top(r);
                let drawn = texts_in(laid, details, top - 1.0, top + rows.cell + 1.0);
                assert!(
                    drawn.first().is_some_and(|d| st.clauses.contains(d)),
                    "{at}: {res:?}'s Details draw {drawn:?}, its state says {:?}",
                    st.clauses
                );
            }
            (None, Some(_)) => rule_s(laid, m, snap, res, (meter, rows, r), at),
            (None, None) => {
                let head = laid
                    .head
                    .as_ref()
                    .unwrap_or_else(|| panic!("{at}: a header"));
                assert!(
                    says_worst(head, snap, ours, block, at),
                    "{at}: its header says it"
                );
            }
        }
    };
    for kind in BlockKind::ALL {
        let w = folded_width(kind, m);
        let at = format!("Folded {kind:?} at {w} pt");
        let p = plan::plan_folded(w, m);
        let laid = lay_at(w, DeckForm::Folded);
        let block = i / 4;
        let b = &p.blocks[block];
        let r = u32::try_from(i % 4).expect("a row");
        let ours: Vec<Resource> = DECK_RESOURCES
            .iter()
            .copied()
            .skip(block * 4)
            .take(4)
            .collect();
        let block = u8::try_from(block).expect("two blocks");
        said(&laid, (b.details, b.meter, b.rows, r), block, &ours, &at);
        out.push((at.clone(), marks(&laid, b.well, b.rows, r)));
        out.push((format!("{at}: header"), labels(&laid, block)));
    }
    for kind in FullKind::ALL {
        let w = full_width(kind, m);
        let at = format!("Full {kind:?} at {w} pt");
        let p = plan::plan_full(w, m);
        let laid = lay_at(w, DeckForm::Full);
        let r = u32::try_from(i).expect("a row");
        said(
            &laid,
            (p.details, p.meter, p.rows, r),
            0,
            &DECK_RESOURCES,
            &at,
        );
        out.push((at.clone(), marks(&laid, p.well, p.rows, r)));
        out.push((format!("{at}: header"), labels(&laid, 0)));
    }
    out
}

/// The CPU's thermal state (A2 §3.2) is said as Memory's pressure is, at every Folded and Full
/// kind: its ● in its well, in `warn` at serious and `critical` at critical, and its words first
/// in its Details, else after its figure run where they fit, else in its block's header, where
/// they read "thermal serious": the machine's level, so they name no row. NEGATIVE CONTROL: at
/// fair nothing is drawn in its well and no header says "thermal"; Memory under pressure draws
/// its ● in its own well by the same reading.
#[test]
fn the_cpu_state_brings_its_header_word_as_memory_does() {
    let m = FaceMetrics::helvetica_neue();
    let calm = scene(Scene::Calm);
    assert!(
        [Resource::Cpu, Resource::Memory]
            .iter()
            .all(|&r| words::state(r, &calm, &MAC).is_none()),
        "CONTROL: the calm scene says no CPU or Memory state"
    );
    let headed = |said: &[(String, Vec<String>)], word: &str| {
        said.iter()
            .filter(|(at, labels)| {
                at.ends_with("header") && labels.iter().any(|l| says_state(l, word))
            })
            .count()
    };
    for (raw, ink) in [(2, InkKind::Warn), (3, InkKind::Critical)] {
        let snap = thermal_scene(raw);
        let st = words::state(Resource::Cpu, &snap, &MAC).expect("a CPU state");
        assert_eq!(st.ink, ink);
        let said = state_said(&snap, Resource::Cpu, &m);
        for (at, marks) in said.iter().filter(|(at, _)| !at.ends_with("header")) {
            assert_eq!(marks, &[WellMark::Dot.word()], "{at}: the CPU well's mark");
        }
        assert!(
            headed(&said, &st.label) >= 1,
            "some kind says {:?} in its header: {said:?}",
            st.label
        );
    }
    let fair = state_said(&thermal_scene(1), Resource::Cpu, &m);
    assert!(
        fair.iter().all(|(_, drawn)| drawn
            .iter()
            .all(|d| !d.contains("thermal") && d != WellMark::Dot.word())),
        "CONTROL: fair draws no mark and says no thermal state: {fair:?}"
    );
    let mut pressed = scene(Scene::Calm);
    pressed.resources[Resource::Memory.index()].state = Some(State::Warn);
    if let Some(mem) = pressed.mem.as_mut() {
        mem.sample.pressure = Pressure::Warn;
    }
    let memory = state_said(&pressed, Resource::Memory, &m);
    for (at, marks) in memory.iter().filter(|(at, _)| !at.ends_with("header")) {
        assert_eq!(
            marks,
            &[WellMark::Dot.word()],
            "CONTROL: {at}: the Memory well's mark"
        );
    }
    assert!(headed(&memory, "Memory warn") >= 1, "CONTROL: {memory:?}");
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
    // A label names its row, but a job's names its jobs and the CPU's the machine's thermal
    // level it says (A2 §3.2).
    let named = match res {
        Resource::Processes => true,
        Resource::Cpu => st.label.starts_with("thermal "),
        _ => st.label.starts_with(words::resource_name(res)),
    };
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
fn deck_gallery() {
    let (dir, _gallery) = crate::scripted_redraw_fixture::gallery_directory("ATERM_DECK_GALLERY");
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
                // A wake is `hollow`, as its sleep is; Increase Contrast's `hollow` is `words2`.
                let hollow = if contrast { inks.words2 } else { inks.hollow };
                let want_rgb = match ev.kind {
                    aterm_vitals::StripEventKind::Crash => inks.critical_text,
                    aterm_vitals::StripEventKind::Restart => inks.warn_text,
                    aterm_vitals::StripEventKind::SleepWake => hollow,
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
                    "{form:?}, contrast {contrast}: {:?} missing X-axis cap pip at {ex}",
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
/// the legend names this Mac with how much of its memory and disk is used of their totals, and
/// no row says a figure past its strip. Full's
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
        let totals = "this Mac \u{b7} 18 cores \u{b7} 42.1 of 128 GiB \u{b7} 1.11 of 2 TB";
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
            for (long, compact) in [
                ("42.1 of 128 GiB used", "42.1/128 GiB used"),
                ("1.11 of 2.00 TB used", "1.11/2.00 TB used"),
            ] {
                assert!(
                    texts.iter().any(|t| *t == long || *t == compact),
                    "{at}: used of total {long:?} in {texts:?}"
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

        // As painted: all five resources keep three visible nested scopes in their bar.
        let [machine_c, aterm_c, tab_c] =
            [inks.machine, inks.aterm, inks.tab].map(|c| rgba(c, 0xFF));
        let mut nested = 0;
        for (ti, (rows, _, bar, under)) in bar_tables(form, w, &m).into_iter().enumerate() {
            let first = if form == DeckForm::Full { 0 } else { ti * 4 };
            for r in 0..rows.count {
                let Some(&res) = DECK_RESOURCES.get(first + r as usize) else {
                    continue;
                };
                if res == Resource::Gpu {
                    continue;
                }
                let horizontal = bar.is_none();
                let col = bar.or(under).expect("CONTROL: scoped resource bar");
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
pub(crate) mod owner_scenes;
