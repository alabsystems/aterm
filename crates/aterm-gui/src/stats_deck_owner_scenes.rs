// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The refined deck's six scenes (2026-10-24): the readings the owner's sketches drew (idle,
//! light, busy, spike, hot-hard, recovering), one snapshot each with its hour, and their gallery
//! at the owner's window.

use super::*;

/// The six scenes the refined deck's sketches drew, calm to struggling and back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Owner {
    /// Not running: calm all hour.
    Idle,
    /// The 0.134 capture's readings.
    Light,
    /// In between: CPU 70% for 20 minutes, nothing waiting.
    Busy,
    /// Pegged over capacity from 5 to 2 minutes ago; 22% now.
    Spike,
    /// Over capacity all hour (load 162 on 18 cores), swapping, pressure warn for 40 minutes.
    HotHard,
    /// Hot-hard for 45 minutes until 6 minutes ago, calm since.
    Recovering,
}

impl Owner {
    pub(crate) const ALL: [Self; 6] = [
        Self::Idle,
        Self::Light,
        Self::Busy,
        Self::Spike,
        Self::HotHard,
        Self::Recovering,
    ];

    /// The sketches' file name.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Light => "light",
            Self::Busy => "busy",
            Self::Spike => "spike",
            Self::HotHard => "hot-hard",
            Self::Recovering => "recovering",
        }
    }
}

/// The owner's window: its deck is 1039 pt wide.
pub(crate) const OWNER_W: f64 = 1039.0;
/// Its terminal cell, pt: the Full deck rounds up to 110 pt.
const OWNER_CELL_PT: f64 = 16.0;
/// The front tab's label in the sketches.
const TAB: &str = "Tab 1";
/// The hour ring's buckets, and the strip's newest 60 of them.
const HOUR: usize = 360;
const SPAN: usize = 60;
/// A heat step's value in the sketches (deck.py `STEP_V`).
const STEP_V: [f64; 7] = [0.02, 0.12, 0.30, 0.50, 0.70, 0.87, 0.97];

/// The sketches' ground, words and accent (deck.py `inks()`).
pub(crate) fn owner_inks() -> VitalsInks {
    ink::vitals_inks([0x30, 0x31, 0x35], [0xD0, 0xD0, 0xD0], [0x50, 0xFA, 0x7B])
}

/// One bucket: its heat as a share of capacity (`None` not read) and its over flag.
type Bucket = (Option<f64>, bool);

/// The cells of a step string, as `tools/strips.py` printed the sketches' strips: `0`–`6` a heat
/// step, `a`–`g` the same step over max (`g` pegged), `.` not read.
fn steps(s: &str) -> Vec<Bucket> {
    s.chars()
        .map(|c| match c {
            '0'..='6' => (Some(STEP_V[c as usize - '0' as usize]), false),
            'a'..='f' => (Some(STEP_V[c as usize - 'a' as usize]), true),
            'g' => (Some(1.0), true),
            _ => (None, false),
        })
        .collect()
}

/// `n` buckets at `heat`, every `over_every`-th (from the second) over max when given.
fn flat(n: usize, heat: f64, over_every: Option<usize>) -> Vec<Bucket> {
    (0..n)
        .map(|i| (Some(heat), over_every.is_some_and(|k| i % k == 1)))
        .collect()
}

/// Six hours of ten-minute cells for `ring`: its ten-minute means, newest last. A ring that holds
/// the whole hour has its hour played again back to six hours, so a scene's 1 h blocks read as its
/// hour does; a shorter one (a series the sketch reads for its strip alone) has nothing older, so
/// its 10 min and 1 h blocks agree that it was not read.
fn long_of(ring: &[Cell]) -> Vec<Cell> {
    let tens: Vec<Cell> = ring
        .rchunks(60)
        .map(|c| Cell::merge(c.iter().copied()))
        .collect();
    let whole = ring.len() >= HOUR;
    let mut long: Vec<Cell> = (0..aterm_vitals::ring::LONG_BUCKETS)
        .map(|i| {
            let at = if whole { i % tens.len().max(1) } else { i };
            tens.get(at).copied().unwrap_or_default()
        })
        .collect();
    long.reverse();
    long
}

/// A row of `cells` (oldest first, the open bucket last) against `cap`, `now` raw.
fn view(now: f64, cap: f64, cells: &[Bucket], aterm: ScopeFig, tab: ScopeFig) -> ResourceView {
    let ring: Vec<Cell> = cells
        .iter()
        .map(|&(h, over)| {
            let v = h.map(|h| h * cap);
            Cell {
                mean: v,
                max: v,
                over: over && v.is_some(),
            }
        })
        .collect();
    ResourceView {
        now: Some(now),
        capacity: Some(cap),
        heat_now: Some((now / cap) as f32),
        over: cells.last().is_some_and(|c| c.1),
        recent: readings_of(&ring, AT),
        long: long_of(&ring).into(),
        long_k: AT.div_euclid(aterm_vitals::ring::LONG_BUCKET_MS),
        ring: ring.into(),
        ring_k: AT.div_euclid(10_000),
        aterm,
        tabs: vec![(FRONT, tab)],
        ..ResourceView::default()
    }
}

fn series(now: f64, cap: f64, cells: &[Bucket], binds: bool) -> Series {
    let ring: Vec<Cell> = cells
        .iter()
        .map(|&(h, over)| {
            let v = h.map(|h| h * cap);
            Cell {
                mean: v,
                max: v,
                over,
            }
        })
        .collect();
    Series {
        now: Some(now),
        capacity: Some(cap),
        recent: readings_of(&ring, AT),
        long: long_of(&ring).into(),
        ring: ring.into(),
        binds,
    }
}

/// What a scene reads, as deck.py's `_scenarios("refined")` and common.py's `_hours` built it.
struct Readings {
    /// CPU: busy share, load average, aterm's and the tab's cores, the strip, the busiest.
    cpu: (f64, f64, f64, f64, &'static str, Busiest),
    /// The CPU hour's 300 buckets before the strip.
    cpu_before: Vec<Bucket>,
    /// GPU: busy share, its memory in use (GiB), the strip.
    gpu: (f64, f64, &'static str),
    /// Memory: GiB used, aterm's and the tab's, the strip, file cache, swap held (GiB), swap
    /// traffic (MiB/s), pressure now.
    mem: (f64, f64, f64, &'static str, f64, f64, Option<f64>, Pressure),
    mem_before: Vec<Bucket>,
    /// Memory's pressure, bucket for bucket over the hour: warn where true.
    warn: Vec<bool>,
    /// Disk: TB used, the strip, aterm's and the tab's writes (B/s), filling (GB/min), growth
    /// over 10 minutes (GB).
    disk: (f64, &'static str, f64, f64, Option<f64>, f64),
    /// Network: the upload's share of its 0.2 MB/s peak, the strip.
    net: (f64, &'static str),
    /// Processes: all, aterm's, the tab's, yours, the strip.
    procs: (f64, f64, f64, f64, &'static str),
    /// Each core's heat step, Super then Performance.
    cores: (&'static str, &'static str),
}

fn busiest(name: &str, n: usize, in_tab: bool, cores: f32) -> Busiest {
    Busiest {
        name: name.into(),
        cores,
        n,
        scope: in_tab.then_some(ProcScope::Tab(FRONT)),
        born_ms: None,
        unborn: n,
    }
}

const LIGHT_CPU: &str = "333222245322233333433222222222214666545333546323222221111110";
const LIGHT_GPU: &str = "000000000000000000000000000000000000000000000011000000000000";
const LIGHT_MEM: &str = "222222222211122222111111111111111222222222222222111211111111";
const LIGHT_NET: &str = "111101101010100000010001000000001100110450000214001201511111";
const DISK_4: &str = "444444444444444444444444444444444444444444444444444444444444";
const ALL_0: &str = "000000000000000000000000000000000000000000000000000000000000";
const ALL_1: &str = "111111111111111111111111111111111111111111111111111111111111";
const HOT_MEM: &str = "5f55f55f55f55f55f55f55f55f55f55f55f55f55f55f55f55f55ffffffff";

fn readings(o: Owner) -> Readings {
    let before = HOUR - SPAN;
    let calm_mem = |heat| flat(before, heat, None);
    let light = Readings {
        cpu: (
            0.046,
            4.1,
            0.37,
            0.10,
            LIGHT_CPU,
            busiest("aterm", 5, false, 0.4),
        ),
        cpu_before: steps(&LIGHT_CPU.repeat(5)),
        gpu: (0.0, 1.1, LIGHT_GPU),
        mem: (
            16.5,
            2.2,
            0.46,
            LIGHT_MEM,
            50.4,
            0.0,
            None,
            Pressure::Normal,
        ),
        mem_before: steps(&LIGHT_MEM.repeat(5)),
        warn: vec![false; HOUR],
        disk: (1.309, DISK_4, 0.1e6, 0.0, Some(1.0), 10.0),
        net: (0.088, LIGHT_NET),
        procs: (743.0, 48.0, 3.0, 481.0, ALL_0),
        cores: ("100000", "211110000000"),
    };
    match o {
        Owner::Light => light,
        Owner::Idle => Readings {
            cpu: (
                0.01,
                0.3,
                0.06,
                0.02,
                ALL_0,
                busiest("WindowServer", 1, false, 0.07),
            ),
            cpu_before: flat(before, 0.012, None),
            gpu: (0.0, 0.4, ALL_0),
            mem: (12.8, 1.9, 0.31, ALL_1, 22.6, 0.0, None, Pressure::Normal),
            mem_before: calm_mem(0.10),
            disk: (1.309, DISK_4, 0.0, 0.0, None, 0.0),
            net: (0.03, ALL_0),
            procs: (702.0, 41.0, 3.0, 462.0, ALL_0),
            cores: ("000000", "000000000000"),
            ..light
        },
        Owner::Busy => Readings {
            cpu: (
                0.70,
                14.0,
                10.9,
                10.6,
                "444444444444444444444444444444444444444444444444444444444444",
                busiest("rustc", 9, true, 9.6),
            ),
            cpu_before: steps(&format!("{}{}", LIGHT_CPU.repeat(4), "4".repeat(60))),
            gpu: (
                0.30,
                3.2,
                "222222222222222222222222222222222222222222222222222222222222",
            ),
            mem: (
                57.6,
                21.4,
                18.9,
                "333333333333333333333333333333333333333333333333333333333333",
                38.2,
                0.0,
                None,
                Pressure::Normal,
            ),
            mem_before: calm_mem(0.40),
            disk: (1.309, DISK_4, 48e6, 45e6, Some(1.4), 14.0),
            net: (
                0.12,
                "111111111111111101111111011111111110111111111111111111111101",
            ),
            procs: (912.0, 126.0, 84.0, 650.0, ALL_1),
            cores: ("455545", "533533444354"),
            ..light
        },
        Owner::Spike => Readings {
            cpu: (
                0.22,
                11.6,
                3.3,
                3.1,
                "333222245322233333433222222222gggggggggggggggggg432222222222",
                busiest("cargo", 4, true, 2.9),
            ),
            cpu_before: steps(&LIGHT_CPU[..30].repeat(10)),
            cores: ("222221", "111211121221"),
            ..light
        },
        Owner::HotHard => Readings {
            cpu: (
                1.0,
                162.4,
                16.6,
                15.9,
                "gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg",
                busiest("clang", 96, true, 12.9),
            ),
            cpu_before: steps(&"g".repeat(before)),
            gpu: (0.12, 2.1, ALL_1),
            mem: (
                112.6,
                71.3,
                64.8,
                HOT_MEM,
                3.1,
                6.0,
                Some(5.2),
                Pressure::Warn,
            ),
            mem_before: flat(before, 0.86, Some(3)),
            warn: (0..HOUR).map(|i| i >= HOUR - 240).collect(),
            disk: (1.36, DISK_4, 210e6, 190e6, Some(2.6), 26.0),
            net: (
                0.04,
                "001000000100011011000000000001000000111100001010110010100010",
            ),
            procs: (897.0, 214.0, 190.0, 655.0, ALL_1),
            cores: ("666666", "666666666666"),
        },
        Owner::Recovering => {
            // Over capacity for 45 minutes (270 buckets): 246 before the strip, 24 in it.
            let run = 270;
            let calm_before = before - (run - SPAN + 36);
            let mut cpu_before = flat(calm_before, 0.06, None);
            cpu_before.extend(steps(&"g".repeat(run - 24)));
            let mut mem_before = flat(calm_before, 0.30, None);
            mem_before.extend(flat(run - 24, 0.88, Some(3)));
            let warn = (0..HOUR)
                .map(|i| (calm_before + 90..calm_before + run).contains(&i))
                .collect();
            Readings {
                cpu: (
                    0.06,
                    2.7,
                    0.4,
                    0.1,
                    "gggggggggggggggggggggggg320110000111110001010100100110110001",
                    busiest("WindowServer", 1, false, 0.3),
                ),
                cpu_before,
                gpu: (
                    0.01,
                    0.6,
                    "111111111111111111111111000000000000000000000000000000000000",
                ),
                mem: (
                    38.4,
                    9.6,
                    4.2,
                    "5f55f55f55f55f55f55f55f5432222222222222222222222222222222222",
                    41.0,
                    5.8,
                    None,
                    Pressure::Normal,
                ),
                mem_before,
                warn,
                disk: (1.35, DISK_4, 0.4e6, 0.0, None, 1.0),
                net: (0.03, ALL_0),
                procs: (760.0, 52.0, 4.0, 488.0, ALL_0),
                cores: ("100111", "000011100000"),
            }
        }
    }
}

/// `o` as one snapshot at [`AT`], its CPU and Memory rows holding the hour.
pub(crate) fn owner_scene(o: Owner) -> Snapshot {
    let r = readings(o);
    let mut s = Snapshot {
        seq: 360,
        at_unix_ms: AT,
        tabs: vec![TabView {
            key: FRONT,
            label: TAB.into(),
            program: "claude".into(),
            ..TabView::default()
        }],
        ..Snapshot::default()
    };
    let f = &mut s.facts;

    let (share, load, aterm, tab, strip, top) = r.cpu;
    let mut cells = r.cpu_before;
    cells.extend(steps(strip));
    s.resources[0] = view(share * 18.0, 18.0, &cells, fig(aterm), fig(tab));
    let core =
        |s: &str| -> Vec<f32> { steps(s).iter().map(|c| c.0.unwrap_or(0.0) as f32).collect() };
    s.cpu = Some(cpu_view(&[
        ("Super", "S", core(r.cores.0)),
        ("Performance", "P", core(r.cores.1)),
    ]));
    f.load1 = Some(load);
    f.busiest = Some(top);

    let (share, gpu_gib, strip) = r.gpu;
    s.resources[1] = view(
        share * 100.0,
        100.0,
        &steps(strip),
        not_per_app(),
        not_per_app(),
    );
    s.gpu = Some(GpuView {
        load: TrendView::flat(share as f32),
        cores: Some(40),
        ..GpuView::default()
    });
    f.gpu = Some(GpuSample {
        device_pct: Some((share * 100.0).round() as u8),
        cores: Some(40),
        in_use_bytes: Some((gpu_gib * GIB) as u64),
        ..GpuSample::default()
    });

    let (used, aterm, tab, strip, cache, swap, swapping, pressure) = r.mem;
    let mut cells = r.mem_before;
    cells.extend(steps(strip));
    let mut mem = view(
        used * GIB,
        128.0 * GIB,
        &cells,
        fig(aterm * GIB),
        fig(tab * GIB),
    );
    mem.pressure = r
        .warn
        .iter()
        .map(|&w| Some(if w { Pressure::Warn } else { Pressure::Normal }))
        .collect::<Vec<_>>()
        .into();
    mem.state = (pressure == Pressure::Warn).then_some(State::Warn);
    s.resources[2] = mem;
    s.mem = Some(MemView {
        sample: MemSample {
            total: (128.0 * GIB) as u64,
            app: ((used - 8.0) * GIB) as u64,
            wired: (4.0 * GIB) as u64,
            compressed: Some((4.0 * GIB) as u64),
            cached: (cache * GIB) as u64,
            free: ((128.0 - used - cache).max(0.0) * GIB) as u64,
            swap_used: (swap * GIB) as u64,
            swap_total: (swap * GIB) as u64,
            pressure,
            commit: None,
            ..MemSample::default()
        },
        ..MemView::default()
    });
    f.swap_rate = swapping.map(|mib| mib * 1_048_576.0);

    let (used_tb, strip, aterm, tab, filling, growth) = r.disk;
    let total = 2_000_000_000_000_u64;
    let used = used_tb * 1e12;
    let avail = total - used as u64;
    s.resources[3] = view(used, total as f64, &steps(strip), fig(aterm), fig(tab));
    s.volume = Some(VolumeSample {
        total,
        free: avail,
        available: avail,
        mount: "/".into(),
    });
    f.disk = Some(DiskFacts {
        level: Some(DiskLevel::Ok),
        warn: Some(350_000_000_000),
        floor: Some(34_000_000_000),
        emergency: Some(8_600_000_000),
        watch: true,
        freed_today: Some((602_000_000_000, AT - 90 * MIN_MS, "build caches".into())),
        ..DiskFacts::default()
    });
    f.disk_rates = Rates {
        rate10: Some(growth * 1e9 / 600.0),
        ..Rates::default()
    };
    f.filling = filling.map(|gb_min| {
        let rate = gb_min * 1e9 / 60.0;
        Filling {
            secs: avail as f64 / rate,
            rate,
            burst: false,
        }
    });

    // The upload binds: its share of a 0.2 MB/s peak is larger than the download's of 9.4.
    let (share, strip) = r.net;
    let up = steps(strip);
    let mut net = view(0.0, 9.4e6, &steps(ALL_0), not_per_app(), not_per_app());
    net.heat_now = Some(share as f32);
    net.second = Some(series(share * 0.2e6, 0.2e6, &up, true));
    net.basis = CapBasis::Peak;
    s.resources[4] = net;
    f.link = Some(LinkSample {
        name: "en7".into(),
        kind: LinkKind::Ethernet,
        up: true,
        counts: true,
        rate_bps: Some(1_000_000_000),
        ..LinkSample::default()
    });
    f.net_read_s = 86_400;

    let (all, aterm, tab, yours, strip) = r.procs;
    let mut procs = view(all, 16_000.0, &steps(strip), fig(aterm), fig(tab));
    procs.second = Some(series(yours, 10_666.0, &[], false));
    s.resources[6] = procs;
    f.pid_counts = (Some(all as u64), Some(yours as u64));
    f.proc_limits = ProcLimits {
        system: Some(16_000),
        per_user: Some(10_666),
    };
    s
}

/// The deck at the owner's window for `snap`: Full, 1039 pt at 2×, 110 pt tall, dark.
pub(crate) fn owner_input(snap: &Snapshot) -> DeckInput<'_> {
    DeckInput {
        network: None,
        network_target: crate::network_deck::Target::Overview,
        snap: Some(snap),
        prev_snap: None,
        tab_panes: vec![FRONT],
        tab_label: TAB.into(),
        tab_count: 1,
        inks: owner_inks(),
        dwc: false,
        marks: Vec::new(),
        plate: None,
        form: DeckForm::Full,
        contrast: false,
        locale: MAC,
        live: crate::stats_live::DeckLive::STILL,
    }
}

pub(crate) fn owner_geom(s: f64) -> DeckGeom {
    let h = deck_px(
        StatsBars::Deck,
        DeckForm::Full,
        s,
        (OWNER_CELL_PT * s) as usize,
    );
    DeckGeom {
        w: (OWNER_W * s) as f32,
        h: h as f32,
        pad: (12.0 * s) as f32,
        scale: s as f32,
        font_px: (13.0 * s) as f32,
    }
}

/// Scene `o` laid at the owner's window on Helvetica Neue's embedded advances, as the tests read
/// it on every host.
fn owner_laid(o: Owner) -> Laid {
    owner_laid_on(o, &FaceMetrics::helvetica_neue())
}

fn owner_laid_on(o: Owner, m: &FaceMetrics) -> Laid {
    let snap = owner_scene(o);
    let input = owner_input(&snap);
    let words = DeckWords::new(&input, m);
    lay(&owner_geom(2.0), &input, &words, m, true)
}

/// The text runs a laid deck draws, left to right within each line, top to bottom.
fn runs(laid: &Laid) -> Vec<String> {
    let mut texts: Vec<(f64, f64, String)> = laid
        .boxes
        .iter()
        .filter(|b| b.kind == BoxKind::Text)
        .map(|b| (b.r[1], b.r[0], b.text.clone()))
        .collect();
    texts.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    texts.into_iter().map(|t| t.2).collect()
}

#[test]
fn the_owner_window_is_110_pt_and_the_glance_tier() {
    let g = owner_geom(2.0);
    assert_eq!((g.w, g.h), (2078.0, 220.0));
    let p = plan::plan_full(OWNER_W, &FaceMetrics::helvetica_neue());
    assert_eq!(
        p.kind,
        FullKind::Wide,
        "the shorter words fit Wide at 1039 pt"
    );
    assert_eq!(p.strip.pitch, 5.0);
    assert!(p.details.is_some_and(|d| d.w >= 120.0));
}

/// The deck says more only while the Mac struggles, in Full and in Folded at the owner's window:
/// the glance's words appear only when unusual, the key shows "over max" only while a strip draws
/// it, and "now" stays beside each live dot but where a state no row has room to say takes its
/// place over the names (the marker's line still marks now down the rows).
#[test]
fn the_deck_says_more_only_when_the_mac_struggles() {
    let m = FaceMetrics::helvetica_neue();
    for form in [DeckForm::Full, DeckForm::Folded] {
        let said: Vec<(Owner, Vec<String>)> = Owner::ALL
            .map(|o| (o, runs(&owner_laid_at(o, form, OWNER_W, &m))))
            .into();
        let chars = |runs: &[String]| -> usize { runs.iter().map(|r| r.chars().count()).sum() };
        let of = |o: Owner| &said.iter().find(|(w, _)| *w == o).expect("a scene").1;
        let calm = chars(of(Owner::Light));
        let ladder = [Owner::Idle, Owner::Light, Owner::Busy, Owner::HotHard].map(|o| chars(of(o)));
        assert!(
            ladder.windows(2).all(|w| w[0] <= w[1]),
            "{form:?}: idle, light, busy, hot-hard say more in turn: {ladder:?}"
        );
        // Full's Details says the unusual clauses; Folded at this width has no Details, so
        // its extra words are the states beside the amounts.
        let (more, how) = if form == DeckForm::Full {
            (13, "a third again")
        } else {
            (11, "a tenth again")
        };
        assert!(
            chars(of(Owner::HotHard)) * 10 > calm * more,
            "{form:?}: hot-hard says {how} what light says: {ladder:?}"
        );
        // What a strip of this form at this width draws: Full's 1h multi-scale horizon, or
        // Folded's multi-scale blocks.
        let form_strips: Vec<crate::stats_plan::Strip> = match form {
            DeckForm::Full => vec![crate::stats_plan::plan_full(OWNER_W, &m).strip],
            DeckForm::Folded => crate::stats_plan::plan_folded(OWNER_W, &m)
                .blocks
                .into_iter()
                .map(|b| b.strip)
                .collect(),
        };
        let period = if form == DeckForm::Full {
            10
        } else {
            crate::stats_plan::FOLD_SLOT_S
        };
        assert_eq!(
            form_strips[0].period_s, period,
            "{form:?}: what a column holds"
        );
        for (o, runs) in &said {
            // The legend shows a mark only while a strip draws it.
            let scene = owner_scene(*o);
            let over_drawn = match form {
                DeckForm::Full => scene.resources[..DECK_RESOURCES.len()].iter().any(|v| {
                    v.heat_multi_scale(form_strips[0].spec, AT, SLOT_COVER_MS, &scene.time_gaps)
                        .iter()
                        .any(|s| s.cell.over)
                }),
                DeckForm::Folded => {
                    // Folded's header queries words.over_shown(&plan.blocks[0].strip), which checks
                    // all non-text rows against block A's multi-scale spec.
                    scene.resources[..DECK_RESOURCES.len()]
                        .iter()
                        .enumerate()
                        .any(|(ri, v)| {
                            aterm_vitals::words::strip_words(DECK_RESOURCES[ri], &scene).is_none()
                                && v.heat_multi_scale(
                                    form_strips[0].spec,
                                    AT,
                                    SLOT_COVER_MS,
                                    &scene.time_gaps,
                                )
                                .iter()
                                .any(|s| s.cell.over)
                        })
                }
            };
            assert_eq!(
                runs.iter().any(|r| r == "over max"),
                over_drawn,
                "{form:?} {o:?}: {runs:?}"
            );
            for gone in [
                "cold",
                "hot",
                "not read",
                "Global",
                "Details",
                "not per app",
                "no per-app figure",
                "busy ",
                "pressure normal",
            ] {
                assert!(
                    !runs.iter().any(|r| r.starts_with(gone)),
                    "{form:?} {o:?} says {gone:?}: {runs:?}"
                );
            }
            // The legend names this Mac, with its totals where no run says each row's maximum.
            assert!(
                runs.iter()
                    .any(|r| r == "this Mac" || r.starts_with("this Mac \u{b7} ")),
                "{form:?} {o:?}: {runs:?}"
            );
            let dots = if form == DeckForm::Full { 1 } else { 2 };
            let head = owner_laid_at(*o, form, OWNER_W, &m).head.expect("a header");
            let stated = head
                .labels
                .iter()
                .filter(|l| matches!(l.label, plan::Label::State(_)))
                .count();
            let nows = runs.iter().filter(|r| *r == "now").count();
            assert!(
                nows <= dots && nows + stated >= dots,
                "{form:?} {o:?}: \"now\" beside each live edge's dot: {runs:?}"
            );
            if matches!(o, Owner::Idle | Owner::Light) {
                assert_eq!(nows, dots, "{form:?} {o:?}: a calm deck says now: {runs:?}");
            }
        }
    }
}

/// Every form at every width says each row's reading once, in one run at the end of its bar
/// (`docs/DESIGN-stats-deck-meter-2026-10-26.md` §3), where a bar at least as long as the run
/// leaves it room: this Mac's reading in the row's unit with the row's maximum; never aterm's or
/// this tab's figures in a run of their own, which the bar's segments show. Where no run stands,
/// the legend names this Mac with its totals where they fit (Folded's at the owner's widths
/// always), so the deck still says each row's maximum. The legend over the bars names the three
/// scopes once.
#[test]
fn the_run_says_the_machines_reading_where_it_fits() {
    let m = FaceMetrics::helvetica_neue();
    let snap = owner_scene(Owner::Light);
    let said = |w: f64, form: DeckForm| -> Vec<String> {
        let input = DeckInput {
            form,
            ..owner_input(&snap)
        };
        let words = DeckWords::new(&input, &m);
        runs(&lay(&owner_geom_at(form, w, 2.0), &input, &words, &m, true))
    };
    let readings = [
        ("0.8", "of 18 cores", "cores"),
        ("16.5", "of 128 GiB", "GiB"),
        ("1.31", "of 2.00 TB", "TB"),
    ];
    let scopes = ["0.4", "0.1", "2.2", "0.5", "48", "3"];
    let (mut with_run, mut without) = (0, 0);
    for (w, form) in [
        (1424.0, DeckForm::Full),
        (OWNER_W, DeckForm::Full),
        (900.0, DeckForm::Full),
        (700.0, DeckForm::Full),
        (550.0, DeckForm::Full),
        (2560.0, DeckForm::Folded),
        (1424.0, DeckForm::Folded),
        (OWNER_W, DeckForm::Folded),
        (940.0, DeckForm::Folded),
    ] {
        let runs = said(w, form);
        let (run, pt) = match form {
            DeckForm::Full => {
                let p = plan::plan_full(w, &m);
                (p.meter.run, p.rows.amount_pt)
            }
            DeckForm::Folded => {
                let p = plan::plan_folded(w, &m);
                (p.blocks[0].meter.run, p.blocks[0].rows.amount_pt)
            }
        };
        for (mac, max, unit) in readings {
            let long = format!("{mac} {max}");
            let short = format!("{mac} {unit}");
            match run {
                Some(run) => {
                    assert!(
                        m.width(&long, pt, false) <= run.w + 1e-9,
                        "{w} pt {form:?}: {long:?} fits the run {run:?}"
                    );
                    assert!(
                        runs.contains(&long),
                        "{w} pt {form:?}: {long:?} in {runs:?}"
                    );
                }
                None => assert!(
                    !runs.iter().any(|r| *r == long || *r == short),
                    "{w} pt {form:?}: no run, yet {mac:?} is said: {runs:?}"
                ),
            }
            assert!(
                !runs.contains(&short),
                "{w} pt {form:?}: {short:?} without its maximum: {runs:?}"
            );
        }
        if run.is_some() {
            with_run += 1;
        } else {
            without += 1;
        }
        let machine: Vec<&String> = runs.iter().filter(|r| r.starts_with("this Mac")).collect();
        assert_eq!(machine.len(), 1, "{w} pt {form:?}: this Mac once: {runs:?}");
        let totals = machine[0].starts_with("this Mac \u{b7} ");
        assert!(
            if run.is_some() {
                !totals
            } else {
                totals || form == DeckForm::Full
            },
            "{w} pt {form:?}: the legend's totals where no run says the maxima: {runs:?}"
        );
        for scope in ["aterm", "Tab 1"] {
            assert_eq!(
                runs.iter().filter(|r| *r == scope).count(),
                1,
                "{w} pt {form:?}: {scope:?} once in the legend: {runs:?}"
            );
        }
        for lone in scopes {
            assert!(
                !runs.iter().any(|r| r == lone),
                "{w} pt {form:?}: a scope's {lone:?} in a run of its own: {runs:?}"
            );
        }
    }
    assert!(
        with_run >= 4 && without >= 3,
        "CONTROL: tables with and without a run: {with_run}, {without}"
    );
}

/// The gauge sits at the Folded deck's top left wherever two slim blocks fit beside it, inside
/// its slot at every tier: 4 pt inside the band, left of its divider, the rows right of it, and
/// its bloom clipped to the slot. Below that (the narrowest tier) the deck draws none, and
/// nothing else moves into its place.
#[test]
fn the_folded_gauge_sits_top_left_inside_its_slot() {
    let m = FaceMetrics::helvetica_neue();
    let snap = owner_scene(Owner::HotHard);
    let input = DeckInput {
        form: DeckForm::Folded,
        ..owner_input(&snap)
    };
    let words = DeckWords::new(&input, &m);
    let (mut drawn, mut bare) = (0, 0);
    for s in [1.0, 2.0] {
        for w in [
            1424.0, 1119.0, OWNER_W, 750.0, 550.0, 450.0, 419.0, 418.0, 350.0,
        ] {
            let plan = plan::plan_folded(w, &m);
            let laid = lay(
                &owner_geom_at(DeckForm::Folded, w, s),
                &input,
                &words,
                &m,
                true,
            );
            let dials: Vec<[f64; 4]> = laid
                .boxes
                .iter()
                .filter(|b| b.kind == BoxKind::Gauge)
                .map(|b| b.r)
                .collect();
            let Some(g) = plan.gauge else {
                assert!(w < 419.0, "{w} pt has room for the gauge");
                assert!(dials.is_empty(), "{w} pt draws no gauge: {dials:?}");
                bare += 1;
                continue;
            };
            drawn += 1;
            assert_eq!(dials, [g.dial()], "{s}x {w} pt");
            let [x0, y0, x1, y1] = g.dial();
            assert!(
                x0 >= 12.0 && y0 >= 4.0 && y1 <= FOLDED_PT - 4.0 && x1 < g.divider,
                "{w} pt: {g:?}"
            );
            assert!(
                plan.blocks[0].name.x > g.divider,
                "{w} pt: the rows right of the gauge"
            );
            for b in laid.boxes.iter().filter(|b| b.kind != BoxKind::Gauge) {
                assert!(
                    b.r[0] >= g.divider || b.r[2] <= x0,
                    "{s}x {w} pt: {:?} {:?} {:?} in the gauge's slot",
                    b.kind,
                    b.text,
                    b.r
                );
            }
            // Hot-hard's bloom stays left of the divider and inside the band.
            let px = |v: f64| (v * s) as f32;
            for p in &laid.prims {
                if let DrawPrim::ClipPush { x, y, w: cw, h } = p
                    && *x < px(g.divider)
                {
                    assert!(
                        *x >= 0.0
                            && *y >= 0.0
                            && x + cw <= px(g.divider) + 1.0
                            && y + h <= px(FOLDED_PT),
                        "{s}x {w} pt: a clip {p:?} past the gauge's slot"
                    );
                }
            }
        }
    }
    assert_eq!(
        (drawn, bare),
        (14, 4),
        "CONTROL: both sides of the gauge's floor"
    );
}

/// The deck at `w` pt and scale `s` in `form`, at the owner's 16 pt cell.
pub(crate) fn owner_geom_at(form: DeckForm, w: f64, s: f64) -> DeckGeom {
    let h = deck_px(StatsBars::Deck, form, s, (OWNER_CELL_PT * s) as usize);
    DeckGeom {
        w: (w * s) as f32,
        h: h as f32,
        pad: (12.0 * s) as f32,
        scale: s as f32,
        font_px: (13.0 * s) as f32,
    }
}

/// Scene `o` laid in `form` at `w` pt, 2×, on `m`.
fn owner_laid_at(o: Owner, form: DeckForm, w: f64, m: &FaceMetrics) -> Laid {
    let snap = owner_scene(o);
    let mut input = owner_input(&snap);
    input.form = form;
    let words = DeckWords::new(&input, m);
    lay(&owner_geom_at(form, w, 2.0), &input, &words, m, true)
}

/// The six scenes for a person to read beside the sketches: Full and Folded at the owner's 1039 pt
/// window, Folded at 750, 550 and 350 pt and Full at 700, 400 and 350 pt, all at 2×, dark as the
/// sketches are and light as nobody sketched them, with each layout's text runs in
/// `owner_text.tsv`. All are laid on the UI face
/// this host installs, as [`deck_paint`] lays the PNG, so the table says what the picture shows: a
/// clause that fits on that face can fall to its short form on the tests' Helvetica Neue advances.
#[test]
#[ignore = "writes PNGs for a person to read: set ATERM_DECK_GALLERY to a directory"]
fn deck_owner_gallery() {
    let Some(dir) = std::env::var_os("ATERM_DECK_GALLERY") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("the gallery directory");
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    // The owner's width in both forms, then narrower ones, where the run gives way to the bar and
    // the bar to its place under the % (`docs/DESIGN-stats-deck-meter-2026-10-26.md` §2).
    let layouts = [
        (DeckForm::Full, OWNER_W),
        (DeckForm::Folded, OWNER_W),
        (DeckForm::Folded, 940.0),
        (DeckForm::Folded, 750.0),
        (DeckForm::Folded, 550.0),
        (DeckForm::Folded, 350.0),
        (DeckForm::Full, 700.0),
        (DeckForm::Full, 400.0),
        (DeckForm::Full, 350.0),
    ];
    let mut tsv = String::from("scene\tform\tpt\truns\tchars\ttext\n");
    let mut written = 0;
    for o in Owner::ALL {
        let snap = owner_scene(o);
        for (form, w) in layouts {
            let name = if form == DeckForm::Full {
                "full"
            } else {
                "folded"
            };
            // The sketches are dark; the light theme is drawn beside them, since nobody
            // sketched it.
            for (theme, inks) in [("dark", owner_inks()), ("light", light_inks())] {
                let input = DeckInput {
                    inks,
                    form,
                    ..owner_input(&snap)
                };
                let g = owner_geom_at(form, w, 2.0);
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
                let file = dir.join(format!("{}_{w}pt_2x_{name}_{theme}.png", o.name()));
                std::fs::write(&file, png).expect("the PNG writes");
                written += 1;
            }
            let said = runs(&owner_laid_at(o, form, w, &deck_metrics()));
            let chars: usize = said
                .iter()
                .filter(|r| r.as_str() != " \u{b7} ")
                .map(|r| r.chars().count())
                .sum();
            tsv.push_str(&format!(
                "{}\t{name}\t{w}\t{}\t{chars}\t{}\n",
                o.name(),
                said.len(),
                said.join(" | ")
            ));
        }
    }
    std::fs::write(dir.join("owner_text.tsv"), tsv).expect("the text table writes");
    assert_eq!(written, Owner::ALL.len() * layouts.len() * 2);
}

/// The owner's 0.144.0 window (`docs/DESIGN-stats-deck-owner-2026-10-26.md`), 1248 pt at 2×,
/// Full and Folded, idle, busy and hot-hard, on Default (the owner's, a green accent), Default
/// Light and Tokyo Night (a blue accent): the frames a person compares before and after.
#[test]
#[ignore = "writes PNGs for a person to read: set ATERM_DECK_GALLERY to a directory"]
fn deck_owner_compare_gallery() {
    let Some(dir) = std::env::var_os("ATERM_DECK_GALLERY") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("the gallery directory");
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut written = 0;
    for o in [Owner::Idle, Owner::Busy, Owner::HotHard] {
        let snap = owner_scene(o);
        for form in [DeckForm::Full, DeckForm::Folded] {
            for (theme, name) in [
                ("Default", "default"),
                ("Default Light", "light"),
                ("Tokyo Night", "tokyo"),
            ] {
                // The steam a frame of the live edge draws: only hot-hard's dial is hot enough.
                let input = DeckInput {
                    inks: theme_inks(theme),
                    form,
                    live: crate::stats_live::DeckLive {
                        steam: Some(GALLERY_STEAM_MS),
                        ..crate::stats_live::DeckLive::STILL
                    },
                    ..owner_input(&snap)
                };
                let g = owner_geom_at(form, 1248.0, 2.0);
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
                let file = dir.join(format!("{form}_{}_{name}.png", o.name()));
                std::fs::write(&file, png).expect("the PNG writes");
                written += 1;
            }
        }
    }
    assert_eq!(written, 18);
    // Hot-hard's steam over the beat's four frames, as the owner's window draws it.
    let snap = owner_scene(Owner::HotHard);
    let mut steam = 0;
    for form in [DeckForm::Full, DeckForm::Folded] {
        for k in 0..4_i64 {
            let input = DeckInput {
                inks: theme_inks("Default"),
                form,
                live: crate::stats_live::DeckLive {
                    steam: Some(GALLERY_STEAM_MS + 250 * k),
                    ..crate::stats_live::DeckLive::STILL
                },
                ..owner_input(&snap)
            };
            let g = owner_geom_at(form, 1248.0, 2.0);
            let (prims, _) = deck_paint(&g, &input);
            let (rgba, pw, ph) =
                crate::tray_raster::rasterize_tray(&prims, g.w as u32, g.h as u32, 1.0, [0; 4]);
            let png =
                crate::app_introspect::encode_rgba8_png(&rgba, pw, ph).expect("the frame encodes");
            let form = if form == DeckForm::Full {
                "full"
            } else {
                "folded"
            };
            let file = dir.join(format!("steam_{form}_hot-hard_{:03}ms.png", 250 * k));
            std::fs::write(&file, png).expect("the PNG writes");
            steam += 1;
        }
    }
    assert_eq!(steam, 8);
}

/// The steam clock the gallery draws its frames at, ms.
const GALLERY_STEAM_MS: i64 = 12_000;

/// The dial steams only while it reads hot (`docs/DESIGN-stats-deck-owner-2026-10-26.md` §5):
/// hot-hard's pinned dial lays its slot as a [`crate::stats_live::LiveLayer::Steam`] rect, the
/// gauge's own, and on the steam's clock draws wisps inside that slot and nowhere else, a frame on
/// drawn on; idle's and busy's dials (0.01 and 0.70, under the hot bloom's 0.78) lay no such rect
/// and draw the same deck at any clock. A steam frame patches the card to the whole raster's
/// bytes, Full and Folded, 1× and 2×, dark and light.
#[test]
fn only_a_hot_dial_steams_inside_its_slot_and_a_steam_frame_patches_byte_for_byte() {
    use crate::stats_live::{DeckLive, LiveLayer};
    let steam_at = |t: i64| DeckLive {
        steam: Some(t),
        ..DeckLive::STILL
    };
    for form in [DeckForm::Full, DeckForm::Folded] {
        for s in [1.0, 2.0] {
            for (ground, inks) in [("dark", owner_inks()), ("light", super::light_inks())] {
                let at = format!("{form:?} {s}x {ground}");
                let g = owner_geom_at(form, 1248.0, s);
                let paint = |o: Owner, live: DeckLive| {
                    let snap = owner_scene(o);
                    let input = DeckInput {
                        inks,
                        form,
                        live,
                        ..owner_input(&snap)
                    };
                    let (prims, _, out) = deck_paint_live(&g, &input);
                    (prims, out)
                };
                let whole = |prims: &[DrawPrim]| {
                    crate::tray_raster::rasterize_tray(prims, g.w as u32, g.h as u32, 1.0, [0; 4])
                };
                let (still, out) = paint(Owner::HotHard, DeckLive::STILL);
                let rect = |layer| out.rects.iter().find(|r| r.0 == layer).map(|r| r.1);
                assert!(rect(LiveLayer::Gauge).is_some(), "{at}: a dial");
                assert_eq!(
                    rect(LiveLayer::Steam),
                    rect(LiveLayer::Gauge),
                    "{at}: hot-hard steams in its dial's slot"
                );
                let (a, _) = paint(Owner::HotHard, steam_at(10_000));
                let (b, _) = paint(Owner::HotHard, steam_at(10_250));
                assert!(a.len() > still.len(), "{at}: the steam draws");
                assert_ne!(
                    format!("{a:?}"),
                    format!("{b:?}"),
                    "{at}: a frame on, it has risen"
                );
                let (base, pw, ph) = whole(&still);
                let (steamy, _, _) = whole(&a);
                let (x, y, w, h) = rect(LiveLayer::Steam).expect("the steam's rect");
                let (x0, y0) = (x.floor().max(0.0) as u32, y.floor().max(0.0) as u32);
                let (x1, y1) = (
                    ((x + w).ceil().max(0.0) as u32).min(pw),
                    ((y + h).ceil().max(0.0) as u32).min(ph),
                );
                let mut shown = 0;
                for py in 0..ph {
                    for px in 0..pw {
                        let i = ((py * pw + px) * 4) as usize;
                        if base[i..i + 4] != steamy[i..i + 4] {
                            assert!(
                                (x0..x1).contains(&px) && (y0..y1).contains(&py),
                                "{at}: the steam drew at ({px}, {py}), outside its slot"
                            );
                            shown += 1;
                        }
                    }
                }
                assert!(shown > 0, "{at}: CONTROL: the steam shows");
                let mut card = crate::SettingsCard {
                    rgba: steamy,
                    pw,
                    ph,
                    dx: 0,
                    dy: 0,
                    fp: 1,
                    geom: 1,
                    raster_fingerprint_once: std::sync::OnceLock::new(),
                    scrim: None,
                };
                assert!(patch_live(
                    &mut card,
                    &b,
                    &out.rects,
                    &steam_at(10_000),
                    &steam_at(10_250)
                ));
                assert!(
                    card.rgba == whole(&b).0,
                    "{at}: the steam's patch is the whole raster's bytes"
                );
                for o in [Owner::Idle, Owner::Busy] {
                    let (cool, out) = paint(o, DeckLive::STILL);
                    assert!(
                        out.rects.iter().all(|r| r.0 != LiveLayer::Steam),
                        "{at} {o:?}: a cool dial lays no steam"
                    );
                    assert!(
                        format!("{:?}", paint(o, steam_at(10_250)).0) == format!("{cool:?}"),
                        "{at} {o:?}: and draws none"
                    );
                }
            }
        }
    }
}

/// The gauge reads each scene as the sketches drew it: the needle the busier engine now (pinned
/// past the redline in hot-hard), the bezel the hour. The soak is [`aterm_vitals::Strain`]'s,
/// an exponential average with a 10 minute time constant (spike 0.21, recovering 0.54), and the
/// bezel draws it on the sketch's scale ([`crate::stats_gauge::drawn_soak`]: 0.12 and 0.43,
/// where the sketch's own 20-minute tally drew 0.12 and 0.50).
#[test]
fn the_gauge_reads_each_scene() {
    use crate::stats_gauge::{Gauge, R_FACE, drawn_soak};
    let want = [
        (Owner::Idle, 0.01, 0.0, 0.0),
        (Owner::Light, 0.046, 0.0, 0.0),
        (Owner::Busy, 0.70, 0.0, 0.0),
        (Owner::Spike, 0.22, 0.21, 0.12),
        (Owner::HotHard, 2.0, 1.0, 1.0),
        (Owner::Recovering, 0.06, 0.54, 0.43),
    ];
    for (o, needle, soak, drawn) in want {
        let g = Gauge::of(&aterm_vitals::Strain::of(&owner_scene(o)));
        let got = g.needle.expect("a reading");
        assert!((got - needle).abs() < 0.005, "{o:?}: needle {got}");
        assert!((g.soak - soak).abs() < 0.01, "{o:?}: soak {}", g.soak);
        let d = drawn_soak(g.soak);
        assert!((d - drawn).abs() < 0.01, "{o:?}: the bezel draws {d}");
        assert_eq!(g.pinned(), o == Owner::HotHard, "{o:?}");
        let laid = owner_laid(o);
        let face = (R_FACE * 2.0) as f32;
        let bloom = laid
            .prims
            .iter()
            .filter(|p| matches!(p, DrawPrim::Ring { r_outer, .. } if *r_outer > face + 0.5))
            .count();
        assert_eq!(bloom > 0, o == Owner::HotHard, "{o:?}: {bloom} bloom rings");
        let gauge = laid
            .boxes
            .iter()
            .filter(|b| b.kind == BoxKind::Gauge)
            .map(|b| b.r)
            .collect::<Vec<_>>();
        assert_eq!(
            gauge,
            [[15.0, 19.5, 77.0, 81.5]],
            "{o:?}: the dial on the six rows"
        );
    }
}

/// Every scope shows its own ink in the bar under a % figure
/// (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §5), read off the raster as a person sees it.
/// The line nests its fills by length alone, each its whole height, so a scope above zero keeps a
/// device px or more of its own ink past the fill inside it, however close the two round and
/// where their shares are equal (the Hot-hard scene's Disk, the machine's 0.68 and aterm's), as
/// the column shows it a rim: in every owner scene, Folded at 550 and 350 pt and Full under
/// slim's width, at 1× and 2×.
#[test]
fn every_scope_shows_its_own_ink_in_the_bar_under_the_percent() {
    let m = FaceMetrics::helvetica_neue();
    let inks = owner_inks();
    let scope_inks = [inks.machine, inks.aterm, inks.tab].map(|c| rgba(c, 0xFF));
    let share = |v: Option<f32>| {
        v.filter(|v| v.is_finite() && *v > 0.0)
            .map_or(0.0, |v| v.clamp(0.0, 1.0))
    };
    let mut shown = 0;
    for o in Owner::ALL {
        let snap = owner_scene(o);
        for (form, w) in [
            (DeckForm::Folded, 450.0),
            (DeckForm::Folded, 350.0),
            (DeckForm::Full, 210.0),
        ] {
            for s in [1.0, 2.0] {
                let at = format!("{o:?} {form:?} {w} pt {s}×");
                let input = DeckInput {
                    form,
                    ..owner_input(&snap)
                };
                let words = DeckWords::new(&input, &m);
                let g = owner_geom_at(form, w, s);
                let laid = lay(&g, &input, &words, &m, true);
                let (raster, pw, _) = crate::tray_raster::rasterize_tray(
                    &laid.prims,
                    g.w as u32,
                    g.h as u32,
                    1.0,
                    [0, 0, 0, 0],
                );
                let pixel = |x: u32, y: u32| {
                    let i = ((y * pw + x) * 4) as usize;
                    [raster[i], raster[i + 1], raster[i + 2], raster[i + 3]]
                };
                for (ti, (rows, _, _, under)) in bar_tables(form, w, &m).into_iter().enumerate() {
                    let col = under.unwrap_or_else(|| panic!("CONTROL {at}: bars under the %"));
                    let first = if form == DeckForm::Full { 0 } else { ti * 4 };
                    for r in 0..rows.count {
                        let Some(&res) = DECK_RESOURCES.get(first + r as usize) else {
                            continue;
                        };
                        let Some(fill) =
                            words.rows.iter().find(|w| w.res == res).and_then(|w| w.bar)
                        else {
                            continue;
                        };
                        let base = px(rows.top(r) + rows.baseline, s);
                        let next = px(rows.top(r + 1), s);
                        let drawn = panels_at(&laid.prims, px(col.x, s), (base, next));
                        let Some(&((x, y, lw, h), _)) = drawn.first() else {
                            panic!("{at} {res:?}: no bar under the %");
                        };
                        // A row of the line clear of its rounded ends' corners.
                        let mid = (y + (h / 2.0).floor()) as u32;
                        let own = scope_inks.map(|ink| {
                            (x as u32..(x + lw) as u32)
                                .filter(|&px| pixel(px, mid) == ink)
                                .count()
                        });
                        let (gs, ats, ts) =
                            (share(fill.global), share(fill.aterm), share(fill.tab));
                        for (k, share) in [gs, ats, ts].into_iter().enumerate() {
                            if share > 0.0 {
                                assert!(
                                    own[k] >= 1,
                                    "{at} {res:?}: {} shows no ink of its own: {own:?} px of \
                                     the machine's, aterm's and this tab's (shares {gs} {ats} {ts})",
                                    ["the machine", "aterm", "this tab"][k]
                                );
                                shown += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(shown > 100, "CONTROL: {shown} scopes above zero");
}
