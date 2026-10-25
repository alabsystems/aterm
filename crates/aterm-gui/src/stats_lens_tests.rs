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
use aterm_vitals::explain::{ActivityId, fixtures};
use aterm_vitals::history::FREE_BUCKET_S;
use aterm_vitals::model::{Hog, ProcScope, ScopeFig, TopProc, Writer};

const LOC: Locale = Locale {
    platform: Platform::MacOs,
    utc_offset_s: -8 * 3600,
};

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
    let writer = |name: &str, bytes_per_s: f64, scope, n| Writer {
        name: name.into(),
        bytes_per_s,
        scope,
        n,
        bytes: bytes_per_s as u64,
    };
    f.writers = vec![
        writer("rustc", 31e6, ProcScope::Tab(3), 9),
        writer("cargo", 5.1e6, ProcScope::Tab(3), 1),
        writer("aterm", 1.7e6, ProcScope::Aterm, 1),
        writer("claude", 0.2e6, ProcScope::Tab(FRONT), 1),
    ];
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
                ..ScopeFig::default()
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

fn input(snap: &Snapshot, inks: VitalsInks) -> DeckInput<'_> {
    let mut input = scene_input(snap, DeckForm::Full);
    input.inks = inks;
    input.locale = LOC;
    input
}

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
        panes: &[FRONT],
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

#[test]
fn a_chart_says_one_hour_ago_in_the_singular() {
    let trace = |n: usize| Trace {
        vals: vec![Some(885.0); n],
        under: DAY,
        min_span: 1.0,
        mark: None,
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

#[test]
fn a_chart_says_an_unread_span_once() {
    let trace = Trace {
        vals: vec![Some(5.0), None, None, Some(7.0)],
        under: HOUR,
        min_span: 1.0,
        mark: None,
    };
    let said = trace_said("Used", "GiB", &trace, (0, 60_000), &LOC);
    assert!(said.ends_with(" Not read from 16:01 to 16:03."), "{said}");
    assert!(!said.contains(", not read"), "{said}");
}

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
            && chart.contains(" Not read from 21:20 to 01:30. ")
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

/// The close-up's busiest program says how long it has been up once it has run long, as a
/// lower bound while a member's birth went unread. NEGATIVE CONTROLS: a young program, and one
/// whose every birth went unread, say no age.
#[test]
fn the_busiest_program_says_how_long_it_has_been_up() {
    let m = FaceMetrics::helvetica_neue();
    let busiest = |born_ago_ms: Option<i64>, unborn: usize| {
        let mut snap = lens_scene(Scene::Calm);
        let at = snap.at_unix_ms;
        let b = snap.facts.busiest.as_mut().expect("a busiest program");
        b.born_ms = born_ago_ms.map(|ago| at - ago);
        b.unborn = unborn;
        busiest_line(two_columns(&snap, &m)).expect("its line")
    };
    assert_eq!(
        busiest(Some(47 * MIN_MS), 0),
        "rustc \u{d7}9 in Tab 3, 5.9 cores, up 47 min"
    );
    assert_eq!(
        busiest(Some(47 * MIN_MS), 1),
        "rustc \u{d7}9 in Tab 3, 5.9 cores, up \u{2265} 47 min",
        "one member's birth unread: the oldest may be older"
    );
    for (young, unborn) in [(Some(5 * MIN_MS), 0), (None, 9)] {
        assert_eq!(
            busiest(young, unborn),
            "rustc \u{d7}9 in Tab 3, 5.9 cores",
            "{young:?}"
        );
    }
}

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

/// "This tab"'s lists (busiest, biggest, writers, jobs) take a process of any of its panes.
#[test]
fn a_split_tabs_lists_take_every_pane() {
    let snap = lens_scene(Scene::Calm);
    let m = FaceMetrics::helvetica_neue();
    let c = Ctx {
        panes: &[4, 1],
        bar: Bar::Tab,
        ..two_columns(&snap, &m)
    };
    assert!(c.in_scope(ProcScope::Tab(4)) && c.in_scope(ProcScope::Tab(1)));
    assert!(!c.in_scope(ProcScope::Tab(5)) && !c.in_scope(ProcScope::Aterm));
}

/// A program in both panes of a split front tab is one row of "this tab": rustc ×2 with both
/// panes' cores, not two rows that each read "rustc in Tab 1".
#[test]
fn a_split_tabs_program_is_one_row() {
    let snap = crate::stats_deck::tests::split_tab_running(7, 8, ["rustc", "rustc"]);
    let input = DeckInput {
        tab_panes: vec![8, 7],
        tab_label: "Tab 1".into(),
        tab_count: 2,
        ..input(&snap, dark_inks())
    };
    let laid = lay_lens(
        &shell(1024.0, 250.0, 2.0),
        &input,
        &Lens {
            bar: Bar::Tab,
            ..pinned(Instrument::Cpu)
        },
        &FaceMetrics::helvetica_neue(),
        false,
    );
    assert_eq!(
        well(&laid, "Busiest now in Tab 1"),
        "rustc \u{d7}2 in Tab 1, 3.0 cores"
    );
}

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
    assert_eq!(chip_ink(&pen, ProcScope::Tab(FRONT), &[FRONT]), inks.tab);
    assert_eq!(chip_ink(&pen, ProcScope::Tab(3), &[FRONT]), inks.aterm);
    assert_eq!(
        chip_ink(&pen, ProcScope::Tab(3), &[FRONT, 3]),
        inks.tab,
        "a split tab's other pane is this tab's"
    );
    assert_eq!(chip_ink(&pen, ProcScope::Aterm, &[FRONT]), inks.aterm);
    assert_eq!(chip_ink(&pen, ProcScope::Other, &[FRONT]), inks.machine);
}

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
    let trouble = lens_scene(Scene::Trouble);
    let names_of = |snap: &Snapshot| -> Vec<String> {
        heard(snap, pinned(Instrument::Cpu))
            .wells
            .into_iter()
            .map(|(n, _)| n)
            .collect()
    };
    assert_eq!(
        names_of(&trouble),
        [
            "Now",
            "Cores, last 60 s",
            "Why now",
            "Busiest now",
            "Load",
            "Over max"
        ],
        "two columns list the left one first: Why now stacks under Cores"
    );
    assert_eq!(
        heard(&trouble, pinned(Instrument::Cpu)).placed[0],
        [(0, 1), (4, 0)],
        "the owner's shell holds Cores at 4 pt a row (93 pt) over Why now's 4 rows (66 pt)"
    );
    let m = FaceMetrics::helvetica_neue();
    let grows = |snap: &Snapshot| cpu_doc(two_columns(snap, &m)).sections[0].forms[0].grow;
    assert!(grows(&snap), "with no Why now, v2's Cores grows");
    let placed = |snap: &Snapshot| {
        let input = input(snap, dark_inks());
        let cpu = pinned(Instrument::Cpu);
        lay_lens(&shell(640.0, 114.0, 2.0), &input, &cpu, &m, false).placed
    };
    let mut v2 = trouble.clone();
    v2.why = None;
    assert_eq!(placed(&trouble), [vec![(0, 0)], vec![(1, 1), (2, 0)]]);
    assert_eq!(placed(&trouble), placed(&v2), "CONTROL: v2's lens");
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
    let mut clients = snap.clone();
    clients.resources[Resource::Gpu.index()].aterm.missing = None;
    let held = heard(&clients, pinned(Instrument::Gpu));
    assert!(
        !well(&held, "Details").contains("no per-app GPU figure"),
        "the client lane holds a figure"
    );
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

#[test]
fn how_to_read_draws_the_cause_mark_in_words2() {
    let snap = lens_scene(Scene::Calm);
    let inks = dark_inks();
    assert!(
        inks.words2 != inks.stuck && inks.words2 != inks.warn_text,
        "CONTROL: the inks differ"
    );
    let laid = lay_lens(
        &shell(1024.0, 250.0, 2.0),
        &input(&snap, inks),
        &pinned(Instrument::HowToRead),
        &FaceMetrics::helvetica_neue(),
        true,
    );
    let mark_before = |label: &str| -> Vec<Rgba> {
        let at = laid
            .prims
            .iter()
            .position(|p| matches!(p, DrawPrim::Text { s, .. } if s == label))
            .unwrap_or_else(|| panic!("{label:?} drawn"));
        laid.prims[..at]
            .iter()
            .rev()
            .map_while(|p| match p {
                DrawPrim::Dot { color, .. } => Some(*color),
                _ => None,
            })
            .collect()
    };
    assert_eq!(
        mark_before("the cause aterm names"),
        [rgba(inks.words2, 0xFF); 3]
    );
    assert_eq!(
        mark_before("memory pressure, a GPU reset"),
        [rgba(inks.warn_text, 0xFF)]
    );
}

fn engine_1032() -> Snapshot {
    engine(fixtures::spotlight_1032())
}

fn engine(scene: fixtures::Scene) -> Snapshot {
    let calm = fixtures::calm_1510();
    let scene = fixtures::Scene {
        mem: calm.mem,
        thermal: calm.thermal,
        ..scene
    };
    let mut s = read(&scene, 3);
    if let Some(cpu) = s.cpu.as_mut() {
        cpu.busy.slow = cpu.busy.level + 1.0;
    }
    s
}

fn read(scene: &fixtures::Scene, windows: u64) -> Snapshot {
    let mut v = aterm_vitals::Vitals::new();
    let mut snaps: Vec<Snapshot> = scene.series(windows).iter().map(|r| v.absorb(r)).collect();
    snaps.pop().expect("the scene's readings")
}

fn cpu_for(snap: &Snapshot, panes: &[u64]) -> LensLaid {
    cpu_lens(snap, panes, 1024.0, false)
}

fn cpu_lens(snap: &Snapshot, panes: &[u64], lw: f64, draw: bool) -> LensLaid {
    let input = DeckInput {
        tab_panes: panes.to_vec(),
        tab_label: "Tab 1".into(),
        ..input(snap, dark_inks())
    };
    lay_lens(
        &shell(lw, 250.0, 2.0),
        &input,
        &pinned(Instrument::Cpu),
        &FaceMetrics::helvetica_neue(),
        draw,
    )
}

fn two_columns<'d>(snap: &'d Snapshot, m: &'d FaceMetrics) -> Ctx<'d> {
    Ctx {
        snap,
        m,
        panes: &[1],
        tab_label: "Tab 1",
        loc: LOC,
        bar: Bar::Global,
        two: true,
        cw: columns(1024.0).1,
    }
}

#[test]
fn why_now_says_what_sys_why_says() {
    let snap = engine_1032();
    let a = account_of(WhySubject::Cpu, &[1], &snap);
    assert!(
        a.nothing_needed && a.you.role == Role::Minor,
        "PRECONDITION: {a:?}"
    );
    let laid = cpu_for(&snap, &[1]);
    let said = well(&laid, "Why now");
    for part in [
        "Why: Spotlight, after start-up, 1.42 cores.",
        "3.30 cores busy: macOS services 2.17 \u{b7} others 0.73 \u{b7} aterm 0.40, Tab 1 0.20.",
        "this tab uses 0.2 of 3.3 cores (6.1%); it is not the cause, easing.",
        "nothing needed",
        "Spotlight by its code signature",
    ] {
        assert!(said.contains(part), "{part:?}: {said}");
    }
    let drawn: Vec<&str> = laid.boxes.iter().map(|b| b.text.as_str()).collect();
    for text in [
        "5 s window \u{b7} exact, 9 coalitions",
        "Spotlight, after start-up",
        "1.42 cores",
        "macOS services 2.17 \u{b7} others 0.73 \u{b7} aterm 0.40, Tab 1 0.20",
        "3.30 cores",
        "this tab uses 0.2 of 3.3 cores (6.1%); it is not the cause \u{b7} easing",
    ] {
        assert!(drawn.contains(&text), "{text:?} is not drawn: {drawn:?}");
    }
    assert!(
        drawn
            .iter()
            .any(|t| t.contains("Spotlight by its code signature")),
        "{drawn:?}"
    );
}

#[test]
fn why_nows_split_adds_up_to_its_busy_figure() {
    assert_eq!(
        hundredths([2175, 735, 200, 190, 0], 3300),
        [218, 73, 20, 19, 0]
    );
    assert_eq!(
        hundredths([2170, 730, 450, 0, -50], 3300),
        [217, 73, 45, 0, -5]
    );
    assert_eq!([cents(217), cents(-5), cents(0)], ["2.17", "-0.05", "0.00"]);
}

#[test]
fn why_nows_split_keeps_whole_figures_when_narrow() {
    let snap = engine_1032();
    let drawn = |lw: f64| -> Vec<String> {
        cpu_lens(&snap, &[1], lw, false)
            .boxes
            .into_iter()
            .map(|b| b.text)
            .collect()
    };
    let whole = "macOS services 2.17 \u{b7} others 0.73 \u{b7} aterm 0.40";
    let narrow = drawn(640.0);
    assert!(narrow.iter().any(|t| t == whole), "{narrow:?}");
    let wide = drawn(1024.0);
    assert!(
        wide.iter().any(|t| *t == format!("{whole}, Tab 1 0.20")),
        "CONTROL: {wide:?}"
    );
}

#[test]
fn why_now_reads_this_tab_by_all_its_panes() {
    let snap = engine_1032();
    let said = |panes: &[u64]| well(&cpu_for(&snap, panes), "Why now").to_owned();
    let both = said(&[5, 1]);
    assert!(
        both.contains("aterm 0.40, Tab 1 0.20.") && both.contains("this tab uses 0.2 of 3.3"),
        "{both}"
    );
    let alone = said(&[5]);
    assert!(
        alone.contains("aterm 0.40, Tab 1 not read.")
            && alone.contains("this tab's share is not read now"),
        "CONTROL: {alone}"
    );
}

#[test]
fn why_now_shows_only_while_a_cause_is_open() {
    let shown = |snap: &Snapshot| {
        cpu_for(snap, &[1])
            .wells
            .iter()
            .any(|(name, _)| name == "Why now")
    };
    let open = engine_1032();
    assert!(shown(&open), "CONTROL: the 10:32 moment shows it");
    let mut none = open.clone();
    none.why = None;
    assert!(!shown(&none));
    let mut stale = open.clone();
    stale.at_ns = open.why.as_ref().map_or(0, |f| f.at_ns) + (CLAUSE_STALE_MS + 1) * 1_000_000;
    assert!(!shown(&stale), "facts older than the clause may ride");
    let cold = read(&fixtures::spotlight_1032(), 1);
    assert!(
        account_of(WhySubject::Cpu, &[1], &cold)
            .causes
            .iter()
            .any(|s| s.cause.id.covering() && s.opened_unix_ms.is_none()),
        "PRECONDITION: Spotlight holds and has not opened"
    );
    assert!(!shown(&cold));
    let calm = read(&fixtures::calm_1510(), 3);
    assert!(calm.why.is_some(), "PRECONDITION: the calm Mac's facts");
    assert!(!shown(&calm));
    let off = engine(fixtures::lane_off_1032());
    assert!(off.why.is_some(), "PRECONDITION: the facts by subtraction");
    assert!(!shown(&off));
}

fn spotlight_failing(more: u64) -> Snapshot {
    let calm = fixtures::calm_1510();
    let scene = fixtures::Scene {
        mem: calm.mem,
        thermal: calm.thermal,
        ..fixtures::spotlight_1032()
    };
    let mut failing = scene.clone();
    for c in failing.coalitions.iter_mut().filter(|c| c.id == 307) {
        c.qos_over_mc = 640;
    }
    let w = scene.window_ms;
    let then = (4..4 + more).flat_map(|k| {
        [
            failing.reading(k * w - failing.sweep_ms, false),
            failing.reading(k * w, true),
        ]
    });
    let mut v = aterm_vitals::Vitals::new();
    scene
        .series(3)
        .into_iter()
        .chain(then)
        .map(|r| v.absorb(&r))
        .last()
        .expect("the readings")
}

#[test]
fn why_now_stays_while_its_episode_is_open_failing() {
    let failing = spotlight_failing(1);
    let a = account_of(WhySubject::Cpu, &[1], &failing);
    let f = a.facts.as_deref().expect("facts");
    assert!(
        f.open
            .iter()
            .any(|o| o.id == CauseId::Activity(ActivityId::Spotlight) && !o.holds)
            && a.headline.map(|h| h.0) == Some(CauseId::Unexplained)
            && a.causes
                .iter()
                .all(|s| !s.cause.id.covering() || s.opened_unix_ms.is_none()),
        "PRECONDITION: Spotlight open, failing; unexplained holds, not open: {a:?}"
    );
    let laid = cpu_for(&failing, &[1]);
    assert!(
        well(&laid, "Why now").starts_with("Why: Spotlight, after start-up, was 1.42 cores. "),
        "{:?}",
        laid.wells
    );
    let drawn: Vec<&str> = laid.boxes.iter().map(|b| b.text.as_str()).collect();
    for text in ["Spotlight, after start-up", "was 1.42 cores"] {
        assert!(drawn.contains(&text), "{text:?} is not drawn: {drawn:?}");
    }
    assert_eq!(
        laid.placed[0],
        [(0, 1), (4, 0)],
        "Cores keeps its fixed form"
    );
    let opened = cpu_for(&spotlight_failing(2), &[1]);
    assert!(
        well(&opened, "Why now").starts_with("no cause aterm can name: macOS services"),
        "CONTROL: open, it is named: {:?}",
        opened.wells
    );
}

#[test]
fn why_now_bounds_the_tab_and_orders_the_split() {
    let mut aterm = fixtures::calm_1510();
    aterm.busy_mc += 1420;
    for c in aterm.coalitions.iter_mut().filter(|c| c.id == 1342) {
        c.mc += 1420;
    }
    for p in aterm.procs.iter_mut().filter(|p| p.pid == 1187) {
        *p = fixtures::Proc::new(p.pid, p.ppid, p.uid, p.coalition, p.birth_ms, "aterm", 1500);
    }
    let mut services = fixtures::tab2_build_1048();
    services.busy_mc += 6000;
    for c in services.coalitions.iter_mut().filter(|c| c.id == 1) {
        c.mc += 6000;
    }
    for p in services.procs.iter_mut().filter(|p| p.pid == 1) {
        *p = fixtures::Proc::new(
            p.pid,
            p.ppid,
            p.uid,
            p.coalition,
            p.birth_ms,
            "launchd",
            6900,
        );
    }
    let cases: [(fixtures::Scene, &[u64], &[&str]); 9] = [
        (
            fixtures::finished_1032(),
            &[1],
            &[
                "no cause aterm can name: aterm, 3.40 cores.",
                "busy: aterm 3.40, Tab 1 at least 0.20 \u{b7} macOS services 2.17 \u{b7} others 0.73.",
                "this tab uses at least 0.2 of 6.3 cores; whether it is the cause is not read now",
                "no step offered yet",
            ],
        ),
        (
            fixtures::reaped_1032(),
            &[1],
            &["\u{b7} unaccounted +0.25."],
        ),
        (
            fixtures::tab_ahead_1032(),
            &[1],
            &["aterm 0.45, Tab 1 0.45 \u{b7} unaccounted -0.05."],
        ),
        (
            fixtures::spotlight_late_1032(),
            &[1],
            &[
                "Why: Spotlight, since wake, 3.42 cores.",
                "busy: others 2.73 \u{b7} macOS services 2.17 \u{b7} aterm 0.40, Tab 1 0.20.",
            ],
        ),
        (
            fixtures::app_work_1510(),
            &[1],
            &["Why: Caffeine \u{b7} Developer ID YD6LEYT6WZ \u{b7} yours, 1.50 cores."],
        ),
        (
            fixtures::tab2_build_1048(),
            &[1],
            &["Why: Tab 2's work, 6.00 cores."],
        ),
        (
            fixtures::tab2_build_1048(),
            &[2],
            &["this tab is the main cause: 6.0 of 8.2 cores, easing."],
        ),
        (services, &[2], &["this tab is part of the cause, easing."]),
        (aterm, &[1], &["Why: aterm itself, 1.50 cores."]),
    ];
    for (scene, panes, parts) in cases {
        let laid = cpu_for(&engine(scene), panes);
        let said = well(&laid, "Why now");
        for part in parts {
            assert!(said.contains(part), "{part:?}: {said}");
        }
    }
}

#[test]
fn why_nows_full_form_marks_only_a_cause_aterm_names() {
    let inks = dark_inks();
    let marks = |snap: &Snapshot, words: &str| -> Vec<Option<Rgba>> {
        let prims = cpu_lens(snap, &[1], 1024.0, true).prims;
        let text = |p: &DrawPrim| matches!(p, DrawPrim::Text { .. });
        let at = prims
            .iter()
            .position(|p| matches!(p, DrawPrim::Text { s, .. } if s == words))
            .unwrap_or_else(|| panic!("{words:?} is not drawn"));
        let from = prims[..at].iter().rposition(text).map_or(0, |k| k + 1);
        prims[from..at]
            .iter()
            .map(|p| match p {
                DrawPrim::Dot { color, .. } => Some(*color),
                _ => None,
            })
            .collect()
    };
    assert_eq!(
        marks(&engine_1032(), "Spotlight, after start-up"),
        [Some(rgba(inks.words2, 0xFF)); 3]
    );
    let unexplained = marks(
        &engine(fixtures::liar_1032()),
        "no cause aterm can name: macOS services, after start-up",
    );
    assert!(unexplained.is_empty(), "{unexplained:?}");
}

#[test]
fn why_nows_one_line_form_draws_the_cause_mark() {
    let snap = engine_1032();
    let m = FaceMetrics::helvetica_neue();
    let inks = dark_inks();
    let c = two_columns(&snap, &m);
    let cw = c.cw;
    let doc = cpu_doc(c);
    let why = doc
        .sections
        .iter()
        .find(|s| s.name == "Why now")
        .expect("Why now");
    let one = why.forms.last().expect("its one-line form");
    assert_eq!(one.label.as_deref(), Some("Why now"));
    let mut pen = Pen {
        p: Painter::new(&inks, 2.0, 26.0, false),
        m: &m,
        inks,
        ic: false,
        s: 2.0,
        ox: 0.0,
        oy: 0.0,
        draw: true,
        line_tab: 0.0,
        boxes: Vec::new(),
    };
    (one.draw)(
        &mut pen,
        Place {
            x: SIDE,
            y: 0.0,
            w: cw,
            h: LINE_H,
        },
    );
    let prims = &pen.p.prims;
    let texts: Vec<(usize, &str, f32, Rgba)> = prims
        .iter()
        .enumerate()
        .filter_map(|(i, p)| match p {
            DrawPrim::Text { s, x, color, .. } => Some((i, s.as_str(), *x, *color)),
            _ => None,
        })
        .collect();
    assert_eq!(
        texts.iter().map(|t| t.1).collect::<Vec<_>>(),
        [
            "Why now",
            "Spotlight, after start-up; this tab is not the cause"
        ]
    );
    assert_eq!(texts[1].3, rgba(inks.words2, 0xFF));
    let dots: Vec<(f32, Rgba)> = prims[texts[0].0 + 1..texts[1].0]
        .iter()
        .map(|p| match p {
            DrawPrim::Dot { cx, color, .. } => (*cx, *color),
            other => panic!("{other:?} between the label and the words"),
        })
        .collect();
    assert_eq!(dots.len(), 3, "{dots:?}");
    assert!(
        dots.iter()
            .all(|&(cx, color)| color == rgba(inks.words2, 0xFF) && cx < texts[1].2),
        "{dots:?}"
    );
}

#[test]
fn the_keys_step_and_copy_the_instruments() {
    let snap = lens_scene(Scene::Calm);
    assert_eq!(
        lens_sentence(&snap, Instrument::Disk, &[FRONT], &LOC),
        words::row_sentence(Resource::Disk, &snap, &[FRONT], &LOC)
    );
    assert_eq!(
        lens_sentence(&snap, Instrument::HowToRead, &[], &LOC),
        how_to_read_words(Platform::MacOs)
    );
    assert!(
        lens_sentence(
            &snap,
            Instrument::HowToRead,
            &[],
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
    for (lw, lh) in [(1024.0, 250.0), (640.0, 250.0), (526.0, 250.0)] {
        shots.push((
            format!("cpu_trouble_{lw}x{lh}"),
            &trouble,
            Instrument::Cpu,
            lw,
            lh,
        ));
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
    assert_eq!(written.len(), (6 + 10 + 3) * 2);
}

/// The card a reading splices (its body over the kept shell's raster) is the card one raster of
/// the whole lens gives, byte for byte.
#[test]
fn the_body_over_the_shells_raster_is_the_whole_lens() {
    use crate::tray_raster::{rasterize_tray, rasterize_tray_over};
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let spotlight = engine_1032();
    let trouble = lens_scene(Scene::Trouble);
    let calm = lens_scene(Scene::Calm);
    for (snap, focus, inks) in [
        (&spotlight, Instrument::Cpu, dark_inks()),
        (&trouble, Instrument::Cpu, light_inks()),
        (&trouble, Instrument::Disk, dark_inks()),
        (&calm, Instrument::Memory, light_inks()),
        (&calm, Instrument::HowToRead, dark_inks()),
    ] {
        let lg = shell(1024.0, 250.0, 2.0);
        let (w, h) = (lg.w as u32, lg.h as u32);
        let laid = lay_lens(
            &lg,
            &input(snap, inks),
            &pinned(focus),
            &deck_metrics(),
            true,
        );
        let (head, body) = laid.prims.split_at(laid.shell);
        assert!(
            head.iter()
                .all(|p| matches!(p, DrawPrim::Panel { .. } | DrawPrim::Stroke { .. })),
            "{focus:?}: the shell is panels and its rim"
        );
        assert!(!body.is_empty(), "{focus:?}");
        let (whole, ..) = rasterize_tray(&laid.prims, w, h, 1.0, [0, 0, 0, 0]);
        let (ground, ..) = rasterize_tray(head, w, h, 1.0, [0, 0, 0, 0]);
        let spliced = rasterize_tray_over(body, 1.0, ground, w, h);
        assert!(whole == spliced, "{focus:?}: the spliced card differs");
    }
}

/// A reading's card drawn with the glyphs the readings before it kept is the card a fresh canvas
/// draws, byte for byte: the CPU close-up (Why now in it) and the Deck, each ink after the other,
/// at both scales; and a second reading of each rasterizes no glyph.
#[test]
fn a_reading_over_kept_glyphs_draws_the_card_a_fresh_canvas_draws() {
    use crate::stats_deck::{DeckGeom, deck_paint, deck_px};
    use crate::tray_raster::{forget_kept_glyphs, glyph_rasters, kept_glyphs, rasterize_tray};
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let spotlight = engine_1032();
    let trouble = lens_scene(Scene::Trouble);
    let mut cards: Vec<(String, Vec<DrawPrim>, u32, u32)> = Vec::new();
    for s in [2.0_f32, 1.0] {
        for (ground, inks) in [("dark", dark_inks()), ("light", light_inks())] {
            for (name, snap) in [("spotlight", &spotlight), ("trouble", &trouble)] {
                let lg = shell(1024.0, 250.0, s);
                let prims = lens_paint(&lg, &input(snap, inks), &pinned(Instrument::Cpu));
                let (w, h) = (lg.w as u32, lg.h as u32);
                cards.push((format!("lens {name} {ground} {s}x"), prims, w, h));
            }
            let deck = input(&trouble, inks);
            let cell = (14.0 * s) as usize;
            let g = DeckGeom {
                w: 1119.0 * s,
                h: deck_px(StatsBars::Deck, DeckForm::Full, f64::from(s), cell) as f32,
                pad: 12.0 * s,
                scale: s,
                font_px: 13.0 * s,
            };
            let (prims, _) = deck_paint(&g, &deck);
            let (w, h) = (g.w as u32, g.h as u32);
            cards.push((format!("deck {ground} {s}x"), prims, w, h));
        }
    }
    let raster = |prims: &[DrawPrim], w: u32, h: u32| rasterize_tray(prims, w, h, 1.0, [0; 4]).0;
    let fresh: Vec<Vec<u8>> = cards
        .iter()
        .map(|(_, prims, w, h)| {
            forget_kept_glyphs();
            raster(prims, *w, *h)
        })
        .collect();
    forget_kept_glyphs();
    for reading in 0..2 {
        for ((name, prims, w, h), fresh) in cards.iter().zip(&fresh) {
            let rasters = glyph_rasters();
            assert!(
                raster(prims, *w, *h) == *fresh,
                "{name}, reading {reading}: the kept glyphs drew another card"
            );
            if reading == 1 {
                assert_eq!(
                    glyph_rasters(),
                    rasters,
                    "{name}: a glyph was rasterized again"
                );
            }
        }
    }
    assert!(kept_glyphs() > 0);
    // Negative control: a card draws its glyphs from the kept ones, so a spoiled one shows.
    crate::tray_raster::spoil_kept_glyphs_for_test();
    let (name, prims, w, h) = &cards[0];
    assert!(
        raster(prims, *w, *h) != fresh[0],
        "{name}: a spoiled kept glyph changed nothing"
    );
    forget_kept_glyphs();
}

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
