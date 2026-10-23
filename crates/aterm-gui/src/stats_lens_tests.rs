// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Layout, text, and gallery tests for the stats lens.

use std::sync::Arc;

use super::*;
use crate::stats_deck::paint_tests::{
    AT, FRONT, GIB, MIN_MS, Scene, light_inks, scene, scene_input,
};
use crate::stats_deck::tests::dark_inks;
use crate::stats_plan::DeckForm;
use aterm_vitals::Cell;
use aterm_vitals::history::FREE_BUCKET_S;
use aterm_vitals::model::{Hog, ProcScope, ScopeFig, TopProc, Writer};

/// The mockups' clock: [`AT`] reads 10:42:17 here.
const LOC: Locale = Locale {
    platform: Platform::MacOs,
    utc_offset_s: -8 * 3600,
};

/// A fixed sequence of numbers in `a..b`.
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

/// An hour's ring for row `i` of `s`: the scene's last ten minutes after
/// fifty minutes that walk to them from `from` (a share of the row's
/// capacity, wandering by `noise`), with two minutes unread at the
/// 40-minutes-ago mark.
fn hour_ring(s: &Snapshot, i: usize, from: f64, noise: f64, r: &mut Rng) -> Arc<[Cell]> {
    let view = &s.resources[i];
    let cap = view.capacity.unwrap_or(1.0);
    let tail: Vec<Cell> = view.ring.to_vec();
    let to = tail.iter().find_map(|c| c.mean).map_or(from, |v| v / cap);
    let n = aterm_vitals::ring::RING_BUCKETS - tail.len();
    let mut cells: Vec<Cell> = (0..n)
        .map(|k| {
            let t = k as f64 / n as f64;
            let v = (from + (to - from) * t + r.uniform(-noise, noise)) * cap;
            let unread = (120..132).contains(&k);
            Cell {
                mean: (!unread).then_some(v),
                max: (!unread).then_some(v),
                over: false,
            }
        })
        .collect();
    cells.extend(tail);
    cells.into()
}

/// Builds a lens test snapshot for `sc`.
pub(crate) fn lens_scene(sc: Scene) -> Snapshot {
    let mut s = scene(sc);
    let now = s.at_unix_ms;
    let mut r = Rng(3);
    let bucket_ms = FREE_BUCKET_S * 1000;
    let now_k = now.div_euclid(bucket_ms);
    let avail = s.volume.as_ref().map_or(0, |v| v.available) as f64 / 1e9;
    let shift = avail - 885.0;
    let mut v = 906.0;
    for j in 0..144_i64 {
        let hrs_ago = 24.0 - j as f64 / 6.0;
        if (9.5..=13.5).contains(&hrs_ago) {
            continue;
        }
        if hrs_ago >= 1.5 {
            v -= 0.30 + r.uniform(-0.08, 0.12);
        }
        if (hrs_ago - 1.5).abs() < 0.09 {
            v = 868.0 + 41.0;
        }
        if hrs_ago < 1.5 {
            v = 909.0 - (1.5 - hrs_ago) * 16.0 + r.uniform(-0.4, 0.4);
        }
        let gb = if j == 143 { 885.0 } else { v };
        let at = ((now_k - 143 + j) * bucket_ms + 1).min(now);
        s.free_history.note(at, ((gb + shift) * 1e9) as u64);
    }
    let f = &mut s.facts;
    f.disk_rates.rate60 = Some(16e9 / 3600.0);
    f.writers = (0..9)
        .map(|_| Writer {
            name: "rustc".into(),
            bytes_per_s: 31e6 / 9.0,
            scope: ProcScope::Tab(3),
        })
        .chain([
            Writer {
                name: "cargo".into(),
                bytes_per_s: 5.1e6,
                scope: ProcScope::Tab(3),
            },
            Writer {
                name: "aterm".into(),
                bytes_per_s: 1.7e6,
                scope: ProcScope::Aterm,
            },
            Writer {
                name: "claude".into(),
                bytes_per_s: 0.2e6,
                scope: ProcScope::Tab(FRONT),
            },
        ])
        .collect();
    if let Some(d) = f.disk.as_mut() {
        d.hogs = vec![
            Hog {
                path: "~/aterm/target".into(),
                bytes: 212_000_000_000,
                partial: false,
                remedy: String::new(),
            },
            Hog {
                path: "~/aterm-worktrees, 9 target folders".into(),
                bytes: 96_000_000_000,
                partial: false,
                remedy: String::new(),
            },
            Hog {
                path: "$HOME/trust/build".into(),
                bytes: 74_000_000_000,
                partial: false,
                remedy: String::new(),
            },
        ];
        d.hogs_at = Some(AT - 2 * MIN_MS);
    }
    let gib = |v: f64| Some((v * GIB) as u64);
    s.top = (0..9)
        .map(|_| TopProc {
            name: "rustc".into(),
            cores: 5.9 / 9.0,
            footprint: gib(0.38),
            scope: ProcScope::Tab(3),
        })
        .chain([
            TopProc {
                name: "aterm".into(),
                cores: 1.2,
                footprint: gib(1.1),
                scope: ProcScope::Aterm,
            },
            TopProc {
                name: "WindowServer".into(),
                cores: 0.9,
                footprint: gib(0.62),
                scope: ProcScope::Other,
            },
            TopProc {
                name: "kernel_task".into(),
                cores: 0.6,
                footprint: None,
                scope: ProcScope::Other,
            },
            TopProc {
                name: "claude".into(),
                cores: 0.1,
                footprint: gib(0.45),
                scope: ProcScope::Tab(FRONT),
            },
        ])
        .collect();
    let cores = |t: &TopProc| Some(f64::from(t.cores));
    let bytes = |t: &TopProc| t.footprint.map(|b| b as f64);
    for (res, of) in [
        (Resource::Cpu, &cores as &dyn Fn(&TopProc) -> Option<f64>),
        (Resource::Memory, &bytes),
    ] {
        let sum = |keep: &dyn Fn(ProcScope) -> bool| -> ScopeFig {
            ScopeFig {
                now: Some(s.top.iter().filter(|t| keep(t.scope)).filter_map(of).sum()),
                missing: None,
            }
        };
        let aterm = sum(&|p| p != ProcScope::Other);
        let tabs = [FRONT, 3].map(|k| (k, sum(&|p| p == ProcScope::Tab(k))));
        let view = &mut s.resources[res.index()];
        view.aterm = aterm;
        view.tabs = tabs.to_vec();
    }
    if let Some(cpu) = s.cpu.as_mut() {
        let n = cpu.total();
        cpu.core_history = (0..n)
            .map(|core| {
                (0..aterm_vitals::model::CORE_HISTORY_LEN)
                    .map(|t| {
                        let base = if core < 6 { 0.9 } else { 0.55 };
                        let wave = 0.4 * ((t as f64 / 6.0) + core as f64).sin();
                        Some((base + wave + r.uniform(-0.1, 0.1)).clamp(0.0, 1.0) as f32)
                    })
                    .collect()
            })
            .collect();
    }
    let mem = hour_ring(&s, 2, 0.31, 0.004, &mut r);
    s.resources[2].ring = mem;
    let procs = hour_ring(&s, 6, 0.054, 0.0008, &mut r);
    s.resources[6].ring = procs;
    if let Some(view) = s.jobs.as_deref() {
        let mut view = view.clone();
        for row in &mut view.rows {
            row.tab = 3;
            row.quiet_ms = Some(match row.verdict {
                aterm_vitals::jobs::Verdict::Stuck => 12 * MIN_MS as u64,
                _ => 4 * MIN_MS as u64,
            });
        }
        s.set_jobs(Some(Arc::new(view)));
    }
    s
}

/// The lens's input: the scene's, at the mockups' clock, in `inks`.
fn input(snap: &Snapshot, inks: VitalsInks) -> DeckInput<'_> {
    let mut input = scene_input(snap, DeckForm::Full);
    input.inks = inks;
    input.locale = LOC;
    input
}

/// A shell `lw` × `lh` pt at `s` device px per pt.
fn shell(lw: f64, lh: f64, s: f32) -> LensGeom {
    let margin = (6.0 * s).round();
    LensGeom {
        dx: 0,
        dy: 0,
        w: (lw as f32 * s) + 2.0 * margin,
        h: (lh as f32 * s) + 2.0 * margin,
        margin,
        scale: s,
        font_px: 13.0 * s,
    }
}

fn pinned(focus: Instrument) -> Lens {
    Lens {
        focus,
        bar: Bar::Global,
        how: Opened::Pinned,
    }
}

/// Every instrument a lens shows.
const ALL: [Instrument; 8] = [
    Instrument::Cpu,
    Instrument::Gpu,
    Instrument::Memory,
    Instrument::Disk,
    Instrument::Network,
    Instrument::Tokens,
    Instrument::Processes,
    Instrument::HowToRead,
];

fn overlap(a: &[f64; 4], b: &[f64; 4]) -> bool {
    a[0] < b[2] - EPS && b[0] < a[2] - EPS && a[1] < b[3] - EPS && b[1] < a[3] - EPS
}

/// What is wrong with one laid lens: a text box outside the shell or over
/// its foot, two boxes that meet, a column count the width does not call
/// for, or a column whose placed sections are not the first of its stack.
fn faults(laid: &LensLaid, lw: f64, lh: f64) -> Vec<String> {
    let mut out = Vec::new();
    for b in &laid.boxes {
        let [x0, y0, x1, y1] = b.r;
        if x0 < -EPS || y0 < -EPS || x1 > lw + EPS || y1 > lh - FOOT + EPS {
            out.push(format!("{:?} leaves the shell at {:?}", b.text, b.r));
        }
    }
    for (i, a) in laid.boxes.iter().enumerate() {
        for b in &laid.boxes[i + 1..] {
            if overlap(&a.r, &b.r) {
                out.push(format!("{:?} meets {:?}", a.text, b.text));
            }
        }
    }
    let want = if lw >= TWO_COLUMNS_PT { 2 } else { 1 };
    if !laid.stacks.is_empty() && laid.stacks.len() != want {
        out.push(format!("{} columns at {lw} pt", laid.stacks.len()));
    }
    for (stack, placed) in laid.stacks.iter().zip(&laid.placed) {
        let ids: Vec<usize> = placed.iter().map(|(i, _)| *i).collect();
        let kept: Vec<usize> = stack.iter().copied().filter(|i| ids.contains(i)).collect();
        if kept != ids {
            out.push(format!("placed {ids:?} out of {stack:?}'s order"));
        }
    }
    let mut shown: Vec<usize> = laid.placed.iter().flatten().map(|(i, _)| *i).collect();
    shown.sort_unstable();
    if shown.iter().enumerate().any(|(k, &i)| k != i) {
        out.push(format!(
            "placed {shown:?} is not a prefix of the priority order"
        ));
    }
    out
}

/// T-G16: at every shell width from 160 to 1024 pt and height from 90 to
/// 250 pt, for every lens, in Helvetica Neue's metrics: one column below
/// 640 pt and two from it, every text box inside the shell and clear of
/// every other, each column's sections in its order, the sections shown the
/// first of the priority order in either column, and Disk's Space now shown
/// at every size.
#[test]
fn every_lens_holds_its_frame_at_every_size() {
    let snap = lens_scene(Scene::Calm);
    let trouble = lens_scene(Scene::Trouble);
    let m = FaceMetrics::helvetica_neue();
    let mut widths: Vec<f64> = (160..=1024).step_by(16).map(f64::from).collect();
    widths.extend([526.0, 639.0, 640.0, 1023.0]);
    let mut heights: Vec<f64> = (90..=250).step_by(8).map(f64::from).collect();
    heights.extend([110.0, 124.0, 134.0, 136.0, 154.0, 249.0]);
    let (snap, trouble, m, widths, heights) = (&snap, &trouble, &m, &widths, &heights);
    let failed: Vec<String> = std::thread::scope(|scope| {
        let runs: Vec<_> = ALL
            .into_iter()
            .map(|focus| {
                scope.spawn(move || {
                    let mut failed = Vec::new();
                    for (name, snap) in [("calm", snap), ("trouble", trouble)] {
                        let input = input(snap, dark_inks());
                        for &lw in widths {
                            for &lh in heights {
                                for how in [Opened::Pinned, Opened::Keyboard, Opened::Hover] {
                                    let lens = Lens {
                                        how,
                                        ..pinned(focus)
                                    };
                                    let laid =
                                        lay_lens(&shell(lw, lh, 2.0), &input, &lens, m, false);
                                    let mut why = faults(&laid, lw, lh);
                                    if focus == Instrument::Disk
                                        && !laid.placed.iter().flatten().any(|(i, _)| *i == 0)
                                    {
                                        why.push("Space now is not shown".into());
                                    }
                                    failed.extend(why.into_iter().map(|why| {
                                        format!("{focus:?} {name} {how:?} {lw}×{lh}: {why}")
                                    }));
                                }
                            }
                            if failed.len() > 20 {
                                return failed;
                            }
                        }
                    }
                    failed
                })
            })
            .collect();
        runs.into_iter()
            .flat_map(|run| {
                run.join()
                    .unwrap_or_else(|_| vec!["a sweep panicked".into()])
            })
            .collect()
    });
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// The Disk lens's sections and the forms they took at `lw` × `lh`, by name.
fn disk_forms(snap: &Snapshot, lw: f64, lh: f64) -> Vec<Vec<(String, usize, usize)>> {
    let m = FaceMetrics::helvetica_neue();
    let input = input(snap, dark_inks());
    let laid = lay_lens(
        &shell(lw, lh, 2.0),
        &input,
        &pinned(Instrument::Disk),
        &m,
        false,
    );
    let c = Ctx {
        snap,
        m: &m,
        tab_key: Some(FRONT),
        tab_label: "Tab 2",
        loc: LOC,
        bar: Bar::Global,
        two: lw >= TWO_COLUMNS_PT,
        cw: columns(lw).1,
    };
    let doc = disk_doc(c);
    laid.placed
        .iter()
        .map(|col| {
            col.iter()
                .map(|&(i, f)| (doc.sections[i].name.clone(), f, doc.sections[i].forms.len()))
                .collect()
        })
        .collect()
}

/// §11.3: at 526 × 250 the Disk lens keeps Space now, the chart and the
/// Where and Writing lines (`dark_1100_lens_disk`); at 526 × 154 Space now
/// and the Filling line (`dark_1100_lens_disk_short`); on a wide shell Space
/// now over the chart on the left, Writing now over Where on the right.
#[test]
fn the_disk_lens_keeps_what_the_mockups_keep() {
    let snap = lens_scene(Scene::Calm);
    let one_line = |(_, form, forms): &(String, usize, usize)| form + 1 == *forms;
    let names = |col: &[(String, usize, usize)]| -> Vec<String> {
        col.iter().map(|(n, ..)| n.clone()).collect()
    };

    let tall = disk_forms(&snap, 526.0, 250.0);
    assert_eq!(tall.len(), 1);
    assert_eq!(
        names(&tall[0]),
        [
            "Space now",
            "GB free, last 24 hours",
            "Where the space is",
            "Writing now"
        ]
    );
    assert!(!one_line(&tall[0][1]), "the chart is kept: {tall:?}");
    assert!(one_line(&tall[0][2]) && one_line(&tall[0][3]), "{tall:?}");

    let short = disk_forms(&snap, 526.0, 154.0);
    assert_eq!(names(&short[0]), ["Space now", "GB free, last 24 hours"]);
    assert!(one_line(&short[0][1]), "the Filling line: {short:?}");

    let wide = disk_forms(&snap, 1024.0, 250.0);
    assert_eq!(wide.len(), 2);
    assert_eq!(names(&wide[0]), ["Space now", "GB free, last 24 hours"]);
    assert!(!one_line(&wide[0][1]), "{wide:?}");
    assert_eq!(names(&wide[1]), ["Writing now", "Where the space is"]);
    assert!(wide[1].iter().all(|s| !one_line(s)), "{wide:?}");
}

/// The words a reader hears for `focus`: its title and its wells.
fn heard(snap: &Snapshot, lens: Lens) -> LensLaid {
    let input = input(snap, dark_inks());
    lay_lens(
        &shell(1024.0, 250.0, 2.0),
        &input,
        &lens,
        &FaceMetrics::helvetica_neue(),
        false,
    )
}

fn well<'a>(laid: &'a LensLaid, name: &str) -> &'a str {
    laid.wells.iter().find(|(n, _)| n == name).map_or_else(
        || panic!("no well {name:?} in {:?}", laid.wells),
        |(_, w)| w,
    )
}

/// A chart's sentence says when its first reading was, in the singular
/// for one: six 10-minute buckets are "1 hour ago", one 1-minute bucket "1
/// minute ago".
#[test]
fn a_chart_says_one_hour_ago_in_the_singular() {
    let trace = |n: usize| Trace {
        vals: vec![Some(885.0); n],
        under: DAY,
        min_span: 1.0,
        mark: None,
        gap: "asleep, not read",
    };
    let said = |n: usize, bucket_ms: i64| {
        trace_said(
            "GB free",
            "gigabytes",
            &trace(n),
            (AT - n as i64 * bucket_ms, bucket_ms),
            &LOC,
        )
    };
    assert_eq!(
        said(6, 600_000),
        "GB free: 885 gigabytes 1 hour ago, 885 now."
    );
    assert_eq!(
        said(12, 600_000),
        "GB free: 885 gigabytes 2 hours ago, 885 now."
    );
    assert_eq!(
        said(1, 60_000),
        "GB free: 885 gigabytes 1 minute ago, 885 now."
    );
    assert_eq!(
        said(5, 60_000),
        "GB free: 885 gigabytes 5 minutes ago, 885 now."
    );
}

/// §7.7 and §12: the Disk lens answers Wrong?, says the 24 hours of free
/// space as a sentence with its gap and aterm's pass, and lists the folders
/// and the writers, the nine rustc as one.
#[test]
fn the_disk_lens_says_its_reading() {
    let snap = lens_scene(Scene::Calm);
    let laid = heard(&snap, pinned(Instrument::Disk));
    assert_eq!(laid.title, "Disk close-up");
    assert!(
        laid.explainer.contains("below 69 GB free"),
        "{}",
        laid.explainer
    );
    assert!(well(&laid, "Wrong?").starts_with("No."));
    let chart = well(&laid, "GB free, last 24 hours");
    assert!(
        chart.starts_with("GB free over the last 24 hours: ")
            && chart.contains(" gigabytes 24 hours ago, 890 now.")
            && chart.contains(", asleep.")
            && chart.ends_with("aterm freed 41 gigabytes at 09:12."),
        "{chart}"
    );
    let at = |s: &str| s.split("Not read from ").nth(1).map(|r| r[..14].to_owned());
    assert_eq!(at(chart).as_deref(), Some("21:20 to 01:30"));
    let mut big = snap.clone();
    if let Some(d) = big.facts.disk.as_mut()
        && let Some(f) = d.freed_today.as_mut()
    {
        f.0 = 1_230_000_000_000;
    }
    let big = heard(&big, pinned(Instrument::Disk));
    let chart = well(&big, "GB free, last 24 hours");
    assert!(
        chart.ends_with("aterm freed 1.2 terabytes at 09:12."),
        "{chart}"
    );
    assert!(well(&laid, "Where the space is").starts_with("~/aterm/target, 212 GB"));
    let writing = well(&laid, "Writing now");
    assert!(
        writing.starts_with("rustc \u{d7}9 in Tab 3, 31 MB/s"),
        "{writing}"
    );
    assert!(writing.ends_with("macOS's own writes and other users' are not counted"));
    assert_eq!(
        well(&laid, "Freed"),
        "aterm 41 GB today (09:12, build caches)"
    );
    let names: Vec<&str> = laid.wells.iter().map(|(n, _)| n.as_str()).collect();
    let at = |name: &str| names.iter().position(|n| *n == name);
    assert_eq!(
        at("Freed"),
        at("Space now").map(|k| k + 1),
        "Freed follows Space now: {names:?}"
    );

    let off = lens_scene(Scene::HarnessOff);
    let laid = heard(&off, pinned(Instrument::Disk));
    assert!(laid.wells.iter().all(|(n, _)| n != "Wrong?"));
    assert!(
        laid.explainer.contains("disk watch is off"),
        "{}",
        laid.explainer
    );
}

/// ↑/↓'s scope filters the lists: Tab shows the front tab's own, aterm
/// every tab's and aterm's, Global everything.
#[test]
fn the_scope_filters_the_lists() {
    let snap = lens_scene(Scene::Calm);
    let at = |bar: Bar, focus: Instrument| {
        heard(
            &snap,
            Lens {
                bar,
                ..pinned(focus)
            },
        )
    };
    let tab = at(Bar::Tab, Instrument::Disk);
    assert!(well(&tab, "Writing now in Tab 2").starts_with("claude in Tab 2, 0.2 MB/s"));
    let aterm = at(Bar::Aterm, Instrument::Cpu);
    let busiest = well(&aterm, "Busiest now in aterm");
    assert!(!busiest.contains("WindowServer"), "{busiest}");
    assert!(busiest.starts_with("rustc \u{d7}9 in Tab 3"), "{busiest}");
    let global = at(Bar::Global, Instrument::Memory);
    assert!(well(&global, "Biggest now").contains("WindowServer"));
}

/// §11.3: a chip is in Tab ink only for this window's front tab; another
/// tab's and aterm's are in aterm ink, everything else in the machine's.
#[test]
fn chips_say_whose_a_row_is() {
    let inks = dark_inks();
    let m = FaceMetrics::helvetica_neue();
    let pen = Pen {
        p: Painter::new(&inks, 2.0, 26.0, false),
        m: &m,
        inks,
        ic: false,
        s: 2.0,
        ox: 0.0,
        oy: 0.0,
        draw: false,
        line_tab: 0.0,
        boxes: Vec::new(),
    };
    assert_eq!(chip_ink(&pen, ProcScope::Tab(FRONT), Some(FRONT)), inks.tab);
    assert_eq!(chip_ink(&pen, ProcScope::Tab(3), Some(FRONT)), inks.aterm);
    assert_eq!(chip_ink(&pen, ProcScope::Aterm, Some(FRONT)), inks.aterm);
    assert_eq!(chip_ink(&pen, ProcScope::Other, Some(FRONT)), inks.machine);
}

/// §11.4: each lens's sections, by heading, on a wide shell.
#[test]
fn each_lens_shows_its_sections() {
    let snap = lens_scene(Scene::Calm);
    let names = |focus: Instrument| -> Vec<String> {
        heard(&snap, pinned(focus))
            .wells
            .into_iter()
            .map(|(n, _)| n)
            .collect()
    };
    assert_eq!(
        names(Instrument::Cpu),
        ["Now", "Cores, last 60 s", "Busiest now", "Load", "Over max"]
    );
    assert_eq!(
        names(Instrument::Memory),
        [
            "Now",
            "Where it is",
            "Used, last hour",
            "Biggest now",
            "Swap",
            "Pressure"
        ]
    );
    assert_eq!(
        names(Instrument::Processes),
        ["Now", "Counts", "Last hour", "Jobs"]
    );
    assert_eq!(
        names(Instrument::HowToRead),
        ["Heat", "Cells", "Scopes", "Marks", "Gestures"]
    );
    for focus in [Instrument::Network, Instrument::Tokens, Instrument::Gpu] {
        assert_eq!(names(focus), ["Now", "Details"], "{focus:?}");
    }
    let cpu = heard(&snap, pinned(Instrument::Cpu));
    assert!(well(&cpu, "Cores, last 60 s").starts_with("6 Super, 12 Performance; "));
    assert!(well(&cpu, "Load").starts_with("25.7 on 18 cores: ~8 waiting"));
    let tokens = heard(&snap, pinned(Instrument::Tokens));
    let details = well(&tokens, "Details");
    assert!(details.contains("No current plan reading"), "{details}");
    assert!(details.contains("Codex, Gemini and agy tabs are not counted"));
    let gpu = heard(&snap, pinned(Instrument::Gpu));
    assert!(well(&gpu, "Details").contains("no per-app GPU figure on macOS"));
    assert!(well(&gpu, "Details").contains("not read"));
    let counter = |count, event| {
        let mut observed = snap.clone();
        observed.facts.gpu.as_mut().expect("GPU sample").recoveries = Some(count);
        observed.facts.gpu_resets = event;
        let laid = heard(&observed, pinned(Instrument::Gpu));
        well(&laid, "Details").to_owned()
    };
    for count in [0, 37] {
        let read = counter(count, None);
        assert!(
            read.contains(&count.to_string()),
            "the observed counter: {read}"
        );
        assert!(!read.contains("not read"), "the counter is known: {read}");
        assert!(!read.contains("this run"), "no delta was observed: {read}");
    }
    let event = counter(37, Some((3, AT - MIN_MS)));
    assert!(event.contains("reset 3"), "the observed increase: {event}");
    assert!(
        !event.contains("counter observed"),
        "the event takes precedence"
    );
    let net = heard(&snap, pinned(Instrument::Network));
    assert!(well(&net, "Details").contains("no per-app network figure"));
    let now = well(&net, "Now");
    assert!(now.starts_with("Network "), "{now}");
}

/// ⌘C copies the instrument's sentence (§12), How to read its own words;
/// ←/→ step over `Instrument::ALL`, wrapping, and leave How to read.
#[test]
fn the_keys_step_and_copy_the_instruments() {
    let snap = lens_scene(Scene::Calm);
    assert_eq!(
        lens_sentence(&snap, Instrument::Disk, Some(FRONT), &LOC),
        words::row_sentence(Resource::Disk, &snap, Some(FRONT), &LOC)
    );
    assert_eq!(
        lens_sentence(&snap, Instrument::HowToRead, None, &LOC),
        how_to_read_words(Platform::MacOs)
    );
    assert!(
        lens_sentence(
            &snap,
            Instrument::HowToRead,
            None,
            &Locale {
                platform: Platform::Linux,
                ..LOC
            }
        )
        .contains("The bar shows this computer in grey"),
        "off macOS the machine is this computer, as the deck names it"
    );
    let mut at = Instrument::Cpu;
    let mut seen = Vec::new();
    for _ in 0..Instrument::ALL.len() {
        seen.push(at);
        at = step_instrument(at, true);
    }
    assert_eq!(seen, Instrument::ALL);
    assert_eq!(at, Instrument::Cpu);
    assert_eq!(
        step_instrument(Instrument::Cpu, false),
        Instrument::Processes
    );
    assert_eq!(
        step_instrument(Instrument::HowToRead, true),
        Instrument::Cpu
    );
    assert_eq!(
        step_instrument(Instrument::HowToRead, false),
        Instrument::Processes
    );
}

/// §11.3's line 1: the longest key hint and source that fit together, the
/// hint's forms tried first; a hover lens says a click pins it.
#[test]
fn line_one_fits_its_longest_forms() {
    let snap = lens_scene(Scene::Calm);
    let m = FaceMetrics::helvetica_neue();
    let line1 = |lw: f64, how: Opened| -> Vec<String> {
        let input = input(&snap, dark_inks());
        let lens = Lens {
            how,
            ..pinned(Instrument::Disk)
        };
        lay_lens(&shell(lw, 250.0, 2.0), &input, &lens, &m, false)
            .boxes
            .iter()
            .filter(|b| (b.r[3] - (LINE1_BASE + DESCENT_EM * NAME_PT)).abs() < 1.0)
            .map(|b| b.text.clone())
            .collect()
    };
    let wide = line1(1024.0, Opened::Pinned);
    assert_eq!(wide[0], "Disk");
    assert_eq!(wide[1], "the startup disk \u{b7} as of 10:42:17");
    assert_eq!(
        &wide[2..],
        [
            "pinned \u{b7} ",
            "resources \u{b7} ",
            "scope \u{b7} esc closes"
        ]
    );
    let narrow = line1(200.0, Opened::Pinned);
    assert_eq!(narrow, ["Disk", "as of 10:42:17", "esc closes"]);
    let hover = line1(1024.0, Opened::Hover);
    assert_eq!(hover.last().map(String::as_str), Some("click pins it"));
}

/// §11.3's drop rule on its own: each section takes its largest form that
/// leaves the one-line forms after it room, else its own one-line form,
/// else drops with every section after it; a kept first section drops the
/// others before it shrinks.
#[test]
fn sections_take_the_largest_form_that_leaves_room() {
    let sec = |hs: &[f64]| Section {
        name: String::new(),
        words: Vec::new(),
        right: false,
        parts: Vec::new(),
        forms: hs.iter().map(|&h| form(h, |_, _| {})).collect(),
    };
    let sections = [
        sec(&[56.0]),
        sec(&[88.0, 72.0, 13.0]),
        sec(&[53.0, 13.0]),
        sec(&[13.0]),
    ];
    let forms = |bottom: f64| -> Vec<usize> {
        lens_body(526.0, 0.0, bottom, &sections, (&[], false))
            .iter()
            .map(|p| p.form)
            .collect()
    };
    assert_eq!(forms(56.0 + 88.0 + 53.0 + 13.0 + 12.0), [0, 0, 0, 0]);
    assert_eq!(forms(56.0 + 72.0 + 13.0 + 13.0 + 12.0), [0, 1, 1, 0]);
    assert_eq!(forms(56.0 + 4.0 + 13.0), [0, 2]);
    assert!(forms(40.0).is_empty());

    let kept = [
        sec(&[56.0, 13.0]),
        sec(&[88.0, 72.0, 13.0]),
        sec(&[53.0, 13.0]),
        sec(&[13.0]),
    ];
    let forms = |bottom: f64, keeps: bool| -> Vec<usize> {
        lens_body(526.0, 0.0, bottom, &kept, (&[], keeps))
            .iter()
            .map(|p| p.form)
            .collect()
    };
    assert_eq!(forms(56.0 + 4.0 + 13.0, true), [0, 2]);
    assert_eq!(forms(56.0 + 4.0 + 13.0, false), [1, 2, 1, 0], "CONTROL");
    assert_eq!(forms(40.0, true), [1, 2]);
}

/// The lens hangs 4 pt under the Deck, 4 pt over the sill, and no lens
/// opens in a window under 160 × 90 pt of grid.
#[test]
fn the_lens_hangs_under_the_deck_and_over_the_sill() {
    let deck = lens_geom(
        StatsBars::Deck,
        80,
        124.0,
        24.0,
        208.0,
        2190.0,
        1652.0,
        2.0,
        26.0,
    )
    .expect("fits");
    assert_eq!(deck.w - 2.0 * deck.margin, 2048.0);
    assert_eq!(deck.h - 2.0 * deck.margin, 500.0);
    assert_eq!(deck.dy as f32 + deck.margin, 80.0 + 124.0 + 8.0);
    let sill = lens_geom(
        StatsBars::Sill,
        1876,
        24.0,
        24.0,
        84.0,
        2190.0,
        1792.0,
        2.0,
        26.0,
    )
    .expect("fits");
    assert_eq!(sill.dy as f32 + sill.margin + 500.0, 1876.0 - 8.0);
    assert!(
        lens_geom(
            StatsBars::Deck,
            0,
            124.0,
            24.0,
            124.0,
            200.0,
            100.0,
            2.0,
            26.0
        )
        .is_none()
    );
}

/// The gallery: every lens at the mockups' sizes, dark and light, in the
/// installed UI face, as PNGs a person reads beside
/// `dark_2238_lens_disk`, `light_2238_lens_disk`, `dark_1100_lens_disk` and
/// `dark_1100_lens_disk_short`.
#[test]
#[ignore = "writes PNGs for a person to read: set ATERM_LENS_GALLERY to a directory"]
fn lens_gallery() {
    let Some(dir) = std::env::var_os("ATERM_LENS_GALLERY") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("the gallery directory");
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let calm = lens_scene(Scene::Calm);
    let trouble = lens_scene(Scene::Trouble);
    let mut shots: Vec<(String, &Snapshot, Instrument, f64, f64)> = Vec::new();
    for (name, snap) in [("calm", &calm), ("trouble", &trouble)] {
        for (lw, lh) in [(1024.0, 250.0), (526.0, 250.0), (526.0, 154.0)] {
            shots.push((
                format!("disk_{name}_{lw}x{lh}"),
                snap,
                Instrument::Disk,
                lw,
                lh,
            ));
        }
    }
    for (name, focus) in [
        ("cpu", Instrument::Cpu),
        ("memory", Instrument::Memory),
        ("processes", Instrument::Processes),
        ("how_to_read", Instrument::HowToRead),
        ("tokens", Instrument::Tokens),
    ] {
        shots.push((format!("{name}_1024x250"), &calm, focus, 1024.0, 250.0));
        shots.push((format!("{name}_526x250"), &calm, focus, 526.0, 250.0));
    }
    let mut written = Vec::new();
    for (name, snap, focus, lw, lh) in shots {
        for (ground, inks, backdrop) in [
            ("dark", dark_inks(), [0x1E, 0x1E, 0x1E, 0xFF]),
            ("light", light_inks(), [0xF5, 0xF5, 0xF5, 0xFF]),
        ] {
            let input = input(snap, inks);
            let lg = shell(lw, lh, 2.0);
            let prims = lens_paint(&lg, &input, &pinned(focus));
            let (rgba, pw, ph) =
                crate::tray_raster::rasterize_tray(&prims, lg.w as u32, lg.h as u32, 1.0, backdrop);
            let png =
                crate::app_introspect::encode_rgba8_png(&rgba, pw, ph).expect("the frame encodes");
            let file = dir.join(format!("lens_{name}_{ground}.png"));
            std::fs::write(&file, png).expect("the PNG writes");
            written.push(file);
        }
    }
    assert_eq!(written.len(), (6 + 10) * 2);
}

/// A CJK LINE OF THE LENS WRAPS BETWEEN ITS CHARACTERS, kinsoku kept, the
/// shared rule's breaks ([`aterm_grapheme::pieces`]): a process name or an
/// explainer with no space is laid on as many lines as its width needs,
/// each inside it, none starting with a closer, rejoined the text; a Latin
/// one wraps at its spaces as before. NEGATIVE CONTROL: broken only at
/// white space, the name was one line cut short with `…`.
#[test]
fn a_cjk_line_of_the_lens_wraps_between_its_characters() {
    let m = FaceMetrics::helvetica_neue();
    let text = "設定ファイルを読み込んでいます。テーマを確認しています。";
    for width in [60.0_f64, 90.0, 120.0] {
        let lines = wrap(&m, text, width, BODY_PT);
        assert!(lines.len() > 1, "{width}: {lines:?}");
        for line in &lines {
            assert!(
                m.width(line, BODY_PT, false) <= width + EPS,
                "{width}: {line:?}"
            );
            assert!(!line.ends_with('\u{2026}'), "{width}: {lines:?}");
        }
        for line in &lines[1..] {
            assert!(!line.starts_with(['。', 'ー']), "{width}: {lines:?}");
        }
        assert_eq!(lines.concat(), text, "{width}");
    }
    assert_eq!(
        wrap(
            &m,
            "one two three",
            m.width("one two", BODY_PT, false),
            BODY_PT
        ),
        ["one two", "three"]
    );
}
