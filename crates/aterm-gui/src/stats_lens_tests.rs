// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Layout, text, and gallery tests for the stats lens.

use std::sync::Arc;

use super::*;
use crate::stats_deck::paint_tests::{
    AT, FRONT, GIB, LONG_TAB, MIN_MS, Scene, light_inks, scene, scene_input,
};
use crate::stats_deck::tests::dark_inks;
use crate::stats_plan::DeckForm;
use aterm_vitals::Cell;
use aterm_vitals::explain::{ActivityId, fixtures};
use aterm_vitals::history::FREE_BUCKET_S;
use aterm_vitals::model::{Hog, ProcScope, ScopeFig, TopProc, VolumeSample, Writer};

const LOC: Locale = Locale {
    platform: Platform::MacOs,
    utc_offset_s: -8 * 3600,
    utc_offset_at: None,
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
            let v = (from + (to - from) * t + r.uniform(-noise, noise)).max(0.0) * cap;
            let unread = (120..132).contains(&k);
            Cell {
                mean: (!unread).then_some(v),
                max: (!unread).then_some(v),
                tau_ms: if unread { 0.0 } else { 10_000.0 },
                over_ms: 0.0,
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
    let avail = s.volume.as_ref().map_or(0, |v| v.free) as f64 / 1e9;
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
        s.free_history.note(
            aterm_vitals::ring::Taken::at(at, 1_000),
            ((gb + shift) * 1e9) as u64,
        );
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

/// [`Scene::Trouble`]'s lens scene with its Memory footprints `by` bytes past the scopes around
/// them: aterm's past used memory and the front tab's past aterm's, so a negative `by` leaves
/// each inside. A `long` front tab bears T-G4's worst name. Without `procs` no process reports
/// its footprint, so Biggest now is the one line of the scopes' figures, as in the owner's scene
/// (the R3 lens fixes' review, 2026-11-06).
fn footprints_scene(by: f64, long: bool, procs: bool) -> Snapshot {
    let mut s = lens_scene(Scene::Trouble);
    if long && let Some(t) = s.tabs.iter_mut().find(|t| t.key == FRONT) {
        t.label = LONG_TAB.into();
    }
    if !procs {
        for t in &mut s.top {
            t.footprint = None;
        }
    }
    let view = &mut s.resources[Resource::Memory.index()];
    let used = view.now.expect("the trouble scene reads memory");
    view.aterm.now = Some(used + by);
    for (key, fig) in &mut view.tabs {
        if *key == FRONT {
            fig.now = Some(used + 2.0 * by);
        }
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

/// The widths and heights the lens laws lay every lens at, pt.
fn lens_sizes() -> (Vec<f64>, Vec<f64>) {
    let mut widths: Vec<f64> = (160..=1024).step_by(16).map(f64::from).collect();
    widths.extend([526.0, 639.0, 640.0, 1023.0]);
    let mut heights: Vec<f64> = (90..=250).step_by(8).map(f64::from).collect();
    heights.extend([110.0, 124.0, 134.0, 136.0, 154.0, 249.0]);
    (widths, heights)
}

/// Every lens keeps its shell, its columns, each column's order and the priority order's prefix
/// at every size, in a calm scene and a troubled one; the Memory lens also where its footprints
/// read larger than the scopes around them, under a short tab name and T-G4's longest, whose
/// words its Biggest heading and line must fit or give up (A1 §4.6), with its processes'
/// footprints read (Biggest now a list) and with none (its one line).
#[test]
fn every_lens_holds_its_frame_at_every_size() {
    let snap = lens_scene(Scene::Calm);
    let trouble = lens_scene(Scene::Trouble);
    let over = footprints_scene(4.0 * GIB, false, true);
    let over_long = footprints_scene(4.0 * GIB, true, true);
    let bare = footprints_scene(4.0 * GIB, false, false);
    let bare_long = footprints_scene(4.0 * GIB, true, false);
    let m = FaceMetrics::helvetica_neue();
    let (widths, heights) = lens_sizes();
    let (snap, trouble, m, widths, heights) = (&snap, &trouble, &m, &widths, &heights);
    let (over, over_long, bare, bare_long) = (&over, &over_long, &bare, &bare_long);
    // How many lenses drew a footprint larger than its parent, with process footprints read and
    // without.
    let said_larger = [
        std::sync::atomic::AtomicUsize::new(0),
        std::sync::atomic::AtomicUsize::new(0),
    ];
    let said_larger = &said_larger;
    let failed: Vec<String> = std::thread::scope(|scope| {
        let runs: Vec<_> = ALL
            .into_iter()
            .map(|focus| {
                scope.spawn(move || {
                    let mut failed = Vec::new();
                    let mut scenes = vec![("calm", snap), ("trouble", trouble)];
                    if focus == Instrument::Memory {
                        scenes.extend([
                            ("footprints over", over),
                            ("footprints over, long tab", over_long),
                            ("footprints over, none of a process", bare),
                            ("footprints over, none of a process, long tab", bare_long),
                        ]);
                    }
                    for (name, snap) in scenes {
                        let input = input(snap, dark_inks());
                        for &lw in widths {
                            for &lh in heights {
                                // How a lens was opened reaches its layout only as the key hint
                                // on line 1, whose room reads the width alone: the keyboard's and
                                // the hover's forms run at the least height and the tallest. A
                                // head that came to lay a hint on another row, or let how it was
                                // opened move what is under it, would escape between them.
                                let hows: &[Opened] = if lh == 90.0 || lh == 250.0 {
                                    &[Opened::Pinned, Opened::Keyboard, Opened::Hover]
                                } else {
                                    &[Opened::Pinned]
                                };
                                for &how in hows {
                                    let lens = Lens {
                                        how,
                                        ..pinned(focus)
                                    };
                                    let laid =
                                        lay_lens(&shell(lw, lh, 2.0), &input, &lens, m, false);
                                    if laid.boxes.iter().any(|b| b.text.contains("larger than")) {
                                        let bare = name.contains("none of a process");
                                        said_larger[usize::from(bare)]
                                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                    }
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
    let said = said_larger
        .each_ref()
        .map(|n| n.load(std::sync::atomic::Ordering::Relaxed));
    assert!(
        said.iter().all(|&n| n > 0),
        "CONTROL: some Memory lens draws a footprint larger than its parent, with process \
         footprints read and without ({said:?})"
    );
}

/// A Memory footprint that crosses the scope around it between two readings changes the words
/// of the Memory lens's Biggest section and nothing else (A1 §4.6): at every size each section
/// keeps its column, place and form, and every other text its words and box, so no column's tab
/// moves and no section above re-forms. The figures as drawn are the same either side; only the
/// words that say a footprint is larger come and go, starting where the words they replace start.
/// It holds with its processes' footprints read, Biggest now a list, and with none, its one line.
/// CONTROL: the Footprints line the follow-up first drew, a section of its own after Biggest
/// while a footprint was larger, moved Swap and Pressure down, the right column's tab from 52 to
/// 64 pt, and Where it is from its bar to one line at 526 × 250 pt; this test fails on it.
#[test]
fn a_footprint_crossing_its_parent_moves_nothing_in_the_memory_lens() {
    let m = FaceMetrics::helvetica_neue();
    let (widths, heights) = lens_sizes();
    let mib = GIB / 1024.0;
    // Where a footprint past its parent was said, with process footprints read and without.
    let mut said = [0_usize; 2];
    let mut failed = Vec::new();
    for (long, procs) in [(false, true), (true, true), (false, false), (true, false)] {
        let (inside, past) = (
            footprints_scene(-mib, long, procs),
            footprints_scene(mib, long, procs),
        );
        let (a_in, b_in) = (input(&inside, dark_inks()), input(&past, dark_inks()));
        for &lw in &widths {
            for &lh in &heights {
                let lay = |i: &DeckInput<'_>| {
                    lay_lens(
                        &shell(lw, lh, 2.0),
                        i,
                        &pinned(Instrument::Memory),
                        &m,
                        false,
                    )
                };
                let (a, b) = (lay(&a_in), lay(&b_in));
                let at = format!("long tab {long}, process footprints {procs}, {lw}×{lh}");
                if a.placed != b.placed {
                    failed.push(format!("{at}: placed {:?} then {:?}", a.placed, b.placed));
                    continue;
                }
                if a.boxes.len() != b.boxes.len() {
                    failed.push(format!(
                        "{at}: {} texts then {}",
                        a.boxes.len(),
                        b.boxes.len()
                    ));
                    continue;
                }
                for (x, y) in a.boxes.iter().zip(&b.boxes) {
                    if y.text.contains("larger than") {
                        said[usize::from(!procs)] += 1;
                        if x.r[..2] != y.r[..2] || x.text.contains("larger than") {
                            failed.push(format!("{at}: {x:?} then {y:?}"));
                        }
                    } else if x != y {
                        failed.push(format!("{at}: {x:?} then {y:?}"));
                    }
                }
            }
        }
    }
    assert!(
        failed.is_empty(),
        "{}",
        failed[..failed.len().min(20)].join("\n")
    );
    assert!(
        said.iter().all(|&n| n > 0),
        "CONTROL: a footprint past its parent is said at some size, with process footprints read \
         and without ({said:?})"
    );
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
            "GB used, last 24 hours",
            "Where the space is",
            "Writing now"
        ]
    );
    assert!(!one_line(&tall[0][1]), "the chart is kept: {tall:?}");
    assert!(one_line(&tall[0][2]) && one_line(&tall[0][3]), "{tall:?}");

    let short = disk_forms(&snap, 526.0, 154.0);
    assert_eq!(names(&short[0]), ["Space now", "GB used, last 24 hours"]);
    assert!(one_line(&short[0][1]), "the Filling line: {short:?}");

    let wide = disk_forms(&snap, 1024.0, 250.0);
    assert_eq!(wide.len(), 2);
    assert_eq!(names(&wide[0]), ["Space now", "GB used, last 24 hours"]);
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
        peaks: Vec::new(),
        heat: Vec::new(),
        under: DAY,
        min_span: 1.0,
        mark: None,
        gaps: Vec::new(),
        events: Vec::new(),
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
        peaks: Vec::new(),
        heat: Vec::new(),
        under: HOUR,
        min_span: 1.0,
        mark: None,
        gaps: Vec::new(),
        events: Vec::new(),
    };
    let said = trace_said("Used", "GiB", &trace, (0, 60_000), &LOC);
    assert!(said.ends_with(" Not read from 16:01 to 16:03."), "{said}");
    assert!(!said.contains(", not read"), "{said}");

    fn dst_offset(unix_s: i64) -> Option<i64> {
        Some(if unix_s < 1_793_523_600 {
            -7 * 3600
        } else {
            -8 * 3600
        })
    }
    let dst_loc = Locale {
        platform: Platform::MacOs,
        utc_offset_s: -8 * 3600,
        utc_offset_at: Some(dst_offset),
    };
    let dst_said = trace_said(
        "Used",
        "GiB",
        &trace,
        (1_793_518_200_000, 3_600_000),
        &dst_loc,
    );
    assert!(
        dst_said.ends_with(" Not read from 01:30 to 02:30."),
        "{dst_said}"
    );
}

/// The Network lens's throughput trace draws the busier direction, column by column (the R3 lens
/// fixes, 2026-11-06): each column's figures are the direction whose share of its own capacity is
/// the larger, the rule the trace's heat, the deck's Network row and the lens's words ("busier
/// direction") take, not receiving alone. Receiving reads 1.5 MB/s all hour, 1.2 % of its 125 MB/s;
/// sending 0.1 MB/s, 0.5 % of its 20 MB/s, until its last 20 minutes at 14 MB/s, 70 %. The trace
/// says receiving's figure an hour ago and sending's now, and draws each column in the heat of its
/// direction's share, the deck row's column heat: 1.2 % an hour ago, 70 % now (the review's gap,
/// 2026-11-06). NEGATIVE CONTROLS: receiving alone says 1.5 now; receiving's heat alone
/// (`heat_cells`) draws now's column at 1.2 %.
#[test]
fn the_network_trace_draws_the_busier_direction() {
    let mut snap = lens_scene(Scene::Calm);
    let view = &mut snap.resources[Resource::Network.index()];
    let n = aterm_vitals::ring::RING_BUCKETS;
    let cell = |v: f64| Cell {
        mean: Some(v),
        max: Some(v),
        tau_ms: 1_000.0,
        over_ms: 0.0,
    };
    view.capacity = Some(125e6);
    view.ring = (0..n).map(|_| cell(1.5e6)).collect();
    view.second = Some(aterm_vitals::model::Series {
        now: Some(14e6),
        capacity: Some(20e6),
        ring: (0..n)
            .map(|k| cell(if k + 120 >= n { 14e6 } else { 0.1e6 }))
            .collect(),
        long: Arc::from([]),
        recent: Arc::from([]),
        binds: true,
    });
    let laid = heard(&snap, pinned(Instrument::Network));
    let chart = well(&laid, "Throughput, last hour");
    assert!(
        chart.starts_with("Throughput over the last hour: 1.5 MB/s 1 hour ago, 14.0 now."),
        "{chart}"
    );
    let traffic = well(&laid, "Traffic now");
    assert!(traffic.starts_with("14 MB/s busier direction"), "{traffic}");
    let m = FaceMetrics::helvetica_neue();
    let c = two_columns(&snap, &m);
    let (trace, unit, _) = net_trace(c);
    assert_eq!(unit, "MB/s");
    let now = snap.at_unix_ms;
    let row = c.view(Resource::Network).heat_columns(60, 60, now);
    for (j, (h, r)) in trace.heat.iter().zip(&row).enumerate() {
        assert_eq!(
            (h.heat, h.over),
            (r.heat, r.over),
            "column {j}: the deck row's column heat"
        );
    }
    let ends = |h: &HeatCell| (h.heat, h.max);
    let (rx, tx) = (Some((1.5e6 / 125e6) as f32), Some((14e6 / 20e6) as f32));
    assert_eq!(
        ends(&trace.heat[0]),
        (rx, rx),
        "receiving's share an hour ago"
    );
    assert_eq!(ends(&trace.heat[59]), (tx, tx), "sending's share now");
}

/// The Network lens's throughput trace against capacities far apart (the R3 lens fixes' review,
/// 2026-11-06): receiving against 100 MB/s, sending against 10, ten minutes at a time. Each column
/// is one direction's minute, so its tick, that direction's largest reading, never stands below its
/// column on the trace's one MB/s axis, and the column and the tick are drawn, at 1× and 2×, each
/// in the heat of its own share: where receiving's 30 MB/s minutes (largest 55, 30 % and 55 %)
/// stand beside sending's 1 (largest 7, 10 % and 70 %), the column is 30 and the tick 55, in the
/// heat of 55 %, a heat step under the deck row's 70 % peak and a step over its column's 30 %;
/// where sending's 6 MB/s minutes (largest 10, 60 % and 100 %) bind, the column is 6 and the tick
/// 10. The shares stand a heat step apart so that the drawn inks tell them apart (that review's own
/// review: at 60 % and 70 % the tick and the row's peak share a step). NEGATIVE CONTROLS: each
/// figure from the series it is the larger share of (the R3 fix as first built) ticks receiving's
/// minutes at sending's 7 MB/s, under their 30 MB/s columns, where no tick is drawn; the tick in
/// the row's peak heat, the larger largest share of either direction, inks receiving's ticks a step
/// too hot, and the drawn check alone finds it (at 60 % it passed); the column in its tick's heat
/// fails the column's check.
#[test]
fn the_network_trace_ticks_each_minute_at_its_own_directions_peak() {
    let mut snap = lens_scene(Scene::Calm);
    let view = &mut snap.resources[Resource::Network.index()];
    let n = aterm_vitals::ring::RING_BUCKETS;
    let ring_k = view.ring_k;
    let cell = |(mean, max): (f64, f64)| Cell {
        mean: Some(mean),
        max: Some(max),
        tau_ms: 10_000.0,
        over_ms: 0.0,
    };
    // Whether sending binds in bucket `i` of the ring: in every other ten minutes of the clock.
    let sends = |i: usize| (ring_k - (n - 1 - i) as i64).div_euclid(60) % 2 == 1;
    view.capacity = Some(100e6);
    view.ring = (0..n).map(|_| cell((30e6, 55e6))).collect();
    view.second = Some(aterm_vitals::model::Series {
        now: Some(1e6),
        capacity: Some(10e6),
        ring: (0..n)
            .map(|i| cell(if sends(i) { (6e6, 10e6) } else { (1e6, 7e6) }))
            .collect(),
        long: Arc::from([]),
        recent: Arc::from([]),
        binds: false,
    });
    let m = FaceMetrics::helvetica_neue();
    let c = two_columns(&snap, &m);
    let (trace, unit, start) = net_trace(c);
    assert_eq!(unit, "MB/s");
    // The deck row's column heat, whose peak takes the larger largest share of either direction.
    let row = c
        .view(Resource::Network)
        .heat_columns(60, 60, snap.at_unix_ms);
    let inks = dark_inks();
    let (mut kinds, mut steps_apart) = ([0_usize; 2], 0_usize);
    for scale in [1.0_f32, 2.0] {
        let pen = Pen {
            p: Painter::new(&inks, scale, 14.0 * scale, false),
            m: &m,
            inks,
            ic: false,
            s: f64::from(scale),
            ox: 0.0,
            oy: 0.0,
            draw: true,
            line_tab: 0.0,
            boxes: Vec::new(),
        };
        // The axis `draw_trace` lays for a plot 56 pt tall.
        let ch = 56.0;
        let read: Vec<f64> = trace
            .vals
            .iter()
            .chain(&trace.peaks)
            .flatten()
            .copied()
            .collect();
        let (mn, mx) = read
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &v| {
                (a.min(v), b.max(v))
            });
        let k = if ch >= 60.0 { 3 } else { 2 };
        let (lo, step) = axis(mn, mx, k, trace.min_span);
        let boxes = trace_boxes(
            &pen,
            &trace,
            (12.0, 30.0, 432.0, 30.0 + ch),
            (lo, lo + step * k as f64),
        );
        assert_eq!(boxes.len(), 60, "{scale}×: every minute read");
        for (j, sky) in boxes.iter().enumerate() {
            let at = format!("{scale}×, column {j}");
            let (v, peak, h) = (trace.vals[j], trace.peaks[j], trace.heat[j]);
            assert!(peak >= v, "{at}: the tick {peak:?} under its column {v:?}");
            let sending = (start / 60_000 + j as i64).div_euclid(10) % 2 == 1;
            let want = if sending {
                ((6.0, 10.0), (Some((6e6 / 10e6) as f32), Some(1.0)))
            } else {
                (
                    (30.0, 55.0),
                    (Some((30e6 / 100e6) as f32), Some((55e6 / 100e6) as f32)),
                )
            };
            assert_eq!(
                (
                    (v.unwrap_or_default(), peak.unwrap_or_default()),
                    (h.heat, h.max)
                ),
                want,
                "{at}: its own direction's minute"
            );
            let Some((_, tick)) = sky.tick else {
                panic!("{at}: no tick at {peak:?} over {v:?}: {sky:?}");
            };
            let (column_heat, tick_heat) = (want.1.0.expect("a share"), want.1.1.expect("a share"));
            assert_eq!(
                sky.column.map(|c| c.1),
                Some(inks.cell_at(column_heat, false)),
                "{at}: the column's own heat"
            );
            assert_eq!(
                tick,
                inks.cell_at(tick_heat, false),
                "{at}: the tick's own heat"
            );
            let row_peak = row[j].max.expect("the row's peak share");
            steps_apart += usize::from(!sending && inks.cell_at(row_peak, false) != tick);
            kinds[usize::from(sending)] += 1;
        }
    }
    assert!(
        kinds.iter().all(|&n| n >= 40) && steps_apart == kinds[0],
        "CONTROL: minutes of each direction ({kinds:?}), and receiving's ticks a heat step from \
         the row's peak ({steps_apart})"
    );
}

/// The Network lens's Traffic line names the busier direction's rate where no capacity is known
/// yet, as the throughput trace beside it does (the R3 lens fixes' review, 2026-11-06): in the
/// first day of learning, on a link that reports no speed, sending 14 MB/s against receiving's
/// 2.1, then a reading whose counters went back, which reads no rates, so the line takes the row's
/// binding series. The view is the one `Vitals::absorb` builds, its binding not set by hand.
/// NEGATIVE CONTROL: binding the first series wherever no share is known (`ResourceView::of`
/// before the review) says "2.1 MB/s busier direction".
#[test]
fn the_traffic_line_names_the_busier_direction_with_no_capacity_known() {
    use aterm_vitals::model::{LinkKind, LinkSample};
    let mut v = aterm_vitals::Vitals::new();
    let mut snap = Snapshot::default();
    for t in 0..6_u64 {
        let mut s = crate::stats_deck::tests::sample_at(10 + t, &[0; 10], &[0, 0]);
        s.at_unix_ms = AT - (5 - t as i64) * 1000;
        s.mono_ms = (10 + t) * 1000;
        s.cadence_ms = 1000;
        // The last reading's counters went back, as a reset leaves them.
        let back = t == 5;
        s.links = Some(vec![LinkSample {
            name: "en0".into(),
            kind: LinkKind::Wifi,
            up: true,
            counts: true,
            rx: if back { 0 } else { 2_100_000 * t },
            tx: if back { 0 } else { 14_000_000 * t },
            rate_bps: None,
            ..LinkSample::default()
        }]);
        snap = v.absorb(&s);
    }
    assert!(
        snap.net.as_ref().is_some_and(|n| n.rates.is_none()),
        "PRECONDITION: no rates this reading"
    );
    let net = &snap.resources[Resource::Network.index()];
    let tx = net.second.as_ref().expect("sending");
    assert_eq!(
        (net.capacity, tx.capacity),
        (None, None),
        "PRECONDITION: no capacity known"
    );
    assert_eq!((net.now, tx.now), (Some(2.1e6), Some(14e6)));
    let laid = heard(&snap, pinned(Instrument::Network));
    let traffic = well(&laid, "Traffic now");
    assert!(traffic.starts_with("14 MB/s busier direction"), "{traffic}");
    let m = FaceMetrics::helvetica_neue();
    let (trace, unit, _) = net_trace(two_columns(&snap, &m));
    assert_eq!(
        (trace.vals.iter().rev().flatten().next().copied(), unit),
        (Some(14.0), "MB/s"),
        "the trace beside it ends at sending's rate"
    );
}

/// The Network lens's Traffic line, folded to its one line where "↓ … · ↑ …" does not fit, names
/// the busier direction with its arrow and its live rate (the second review of the R3 lens fixes,
/// 2026-11-06): the direction that binds, by the larger share of its own capacity, as the deck
/// row's arrow, the Now line and the throughput trace beside it take it, not the larger raw rate.
/// Peak basis on asymmetric links, each direction binding in turn, once with the larger raw rate
/// and once with the smaller (the second review's own review, 2026-11-06): the binding direction's
/// 4 MB/s of its 5 MB/s peak (80 %) beside the other's 10 of its 50 (20 %), and its 40 of 50
/// (80 %) beside the other's 1 of 5 (20 %). Laid at every lens size from 160 to 320 pt wide, where
/// the line takes its full form at some and its short form at others. NEGATIVE CONTROLS: the
/// larger raw rate (`rx.max(tx)`, as main drew it) says "10 MB/s" beside a trace that ends at
/// sending's 4.0; sending's arrow always, as one scene where sending binds could not tell from the
/// rule, says "↑ 10 MB/s" where receiving's 4.0 binds; the smaller raw rate's direction says
/// "↑ 1.0 MB/s" where receiving's 40 binds.
#[test]
fn the_traffic_lines_short_form_names_the_busier_direction() {
    let n = aterm_vitals::ring::RING_BUCKETS;
    // A ring bucket read through its 10 s.
    let cell = |v: f64| Cell {
        mean: Some(v),
        max: Some(v),
        tau_ms: 10_000.0,
        over_ms: 0.0,
    };
    let m = FaceMetrics::helvetica_neue();
    // `(rx, tx, rx_cap, tx_cap)`, B/s.
    let links = [
        (10e6, 4e6, 50e6, 5e6),
        (4e6, 10e6, 5e6, 50e6),
        (40e6, 1e6, 50e6, 5e6),
        (1e6, 40e6, 5e6, 50e6),
    ];
    for (rx, tx, rx_cap, tx_cap) in links {
        let mut snap = lens_scene(Scene::Calm);
        let sending = tx / tx_cap > rx / rx_cap;
        let view = &mut snap.resources[Resource::Network.index()];
        view.basis = aterm_vitals::model::CapBasis::Peak;
        view.capacity = Some(rx_cap);
        view.now = Some(rx);
        view.heat_now = Some((tx / tx_cap).max(rx / rx_cap) as f32);
        view.ring = (0..n).map(|_| cell(rx)).collect();
        view.second = Some(aterm_vitals::model::Series {
            now: Some(tx),
            capacity: Some(tx_cap),
            ring: (0..n).map(|_| cell(tx)).collect(),
            long: Arc::from([]),
            recent: Arc::from([]),
            // `ResourceView::of`'s rule: the larger share binds.
            binds: sending,
        });
        snap.net = Some(aterm_vitals::NetView {
            rates: Some(aterm_vitals::LinkRates {
                rx_bps: rx,
                tx_bps: tx,
                rx_pps: 6_000.0,
                tx_pps: 1_000.0,
                ..aterm_vitals::LinkRates::default()
            }),
            state: aterm_vitals::NetState::Online,
            ..aterm_vitals::NetView::default()
        });
        let (arrow, mark, rate) = if sending {
            ('\u{2191}', words::Mark::Up, tx)
        } else {
            ('\u{2193}', words::Mark::Down, rx)
        };
        let tag = format!("receiving {rx} of {rx_cap} B/s, sending {tx} of {tx_cap}");
        let view = &snap.resources[Resource::Network.index()];
        assert_eq!(
            words::well(Resource::Network, view).map(|w| w.0),
            Some(mark),
            "PRECONDITION {tag}: the deck row's arrow is the binding direction's"
        );
        let (trace, unit, _) = net_trace(two_columns(&snap, &m));
        assert_eq!(
            (trace.vals.iter().rev().flatten().next().copied(), unit),
            (Some(rate / 1e6), "MB/s"),
            "PRECONDITION {tag}: the trace ends at the binding direction's rate"
        );
        let full = format!(
            "\u{2193} {} \u{b7} \u{2191} {}",
            net_rate_word(rx),
            net_rate_word(tx)
        );
        let short = format!("{arrow} {}", net_rate_word(rate));
        let input = input(&snap, dark_inks());
        let mut forms = [0_usize; 2];
        for lw in (160..=320).step_by(4).map(f64::from) {
            for lh in (90..=250).step_by(8).map(f64::from) {
                let laid = lay_lens(
                    &shell(lw, lh, 2.0),
                    &input,
                    &pinned(Instrument::Network),
                    &m,
                    false,
                );
                let Some(label) = laid.boxes.iter().find(|b| b.text == "Traffic") else {
                    continue;
                };
                let Some(line) = laid
                    .boxes
                    .iter()
                    .find(|b| b.r[0] > label.r[2] && b.r[1] < label.r[3] && label.r[1] < b.r[3])
                else {
                    panic!("{tag}, {lw}×{lh}: the Traffic line's words beside its label");
                };
                if line.text == full {
                    forms[0] += 1;
                    continue;
                }
                assert!(
                    line.text == short
                        || (line.text.ends_with('\u{2026}') && line.text.starts_with(arrow)),
                    "{tag}, {lw}×{lh}: the Traffic line says {:?}, not the busier direction's \
                     {short:?}",
                    line.text
                );
                forms[1] += usize::from(line.text == short);
            }
        }
        assert!(
            forms.iter().all(|&n| n > 50),
            "CONTROL {tag}: the Traffic line laid in its full form ({}) and its short form ({})",
            forms[0],
            forms[1]
        );
    }
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
    let chart = well(&laid, "GB used, last 24 hours");
    assert!(
        chart.starts_with("GB used over the last 24 hours: ")
            && chart.contains(" gigabytes 24 hours ago, 1110 now.")
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
    let chart = well(&big, "GB used, last 24 hours");
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

fn descendant_jobs_scene(descendant: Option<(u32, u64)>) -> Snapshot {
    use crate::stats_paint::tests::job_row;
    use aterm_vitals::jobs::{Class, JobKind, JobsView, Verdict};

    let mut snap = lens_scene(Scene::Calm);
    let mut wrapper = job_row("wrapper", Verdict::Stuck, Some(18_900), Some(2_612_271));
    wrapper.kind = JobKind::AgentCall;
    wrapper.class = Class::Polling;
    wrapper.exe = "observer".into();
    wrapper.working_descendant = descendant;
    let mut independent = job_row("independent", Verdict::Stuck, Some(9_000), Some(480_000));
    independent.class = Class::Waiting;
    independent.exe = "other-job".into();
    snap.set_jobs(Some(Arc::new(JobsView {
        rows: vec![wrapper, independent],
        ..JobsView::default()
    })));
    snap
}

#[test]
fn jobs_keep_child_activity_beside_local_stalls_in_both_layouts() {
    let m = FaceMetrics::helvetica_neue();
    for descendant in [None, Some((84682, 1_793_674_154_348_639)), None] {
        let snap = descendant_jobs_scene(descendant);
        let rows = snap.jobs.as_ref().unwrap().rows.clone();
        for (width, detailed) in [(1024.0, true), (526.0, false)] {
            let laid = lay_lens(
                &shell(width, 250.0, 2.0),
                &input(&snap, dark_inks()),
                &pinned(Instrument::Processes),
                &m,
                true,
            );
            let spoken = well(&laid, "Jobs");
            assert_eq!(spoken.contains("84682"), descendant.is_some(), "{spoken}");
            let drawn = laid
                .boxes
                .iter()
                .map(|b| b.text.as_str())
                .collect::<Vec<_>>()
                .join(" | ");
            assert_eq!(
                drawn.contains("child activity"),
                descendant.is_some(),
                "{width}: {drawn}"
            );
            assert_eq!(
                drawn.contains("local"),
                descendant.is_some(),
                "{width}: {drawn}"
            );
            assert!(spoken.contains("other-job"), "{width}: {spoken}");
            if detailed {
                assert!(drawn.contains("other-job"), "{width}: {drawn}");
            } else {
                let independent = if descendant.is_some() { 1 } else { 2 };
                assert!(
                    drawn.contains(&format!(
                        "{independent} {}",
                        aterm_vitals::jobs::Verdict::Stuck.word()
                    )),
                    "{width}: {drawn}"
                );
            }
            assert_eq!(snap.jobs.as_ref().unwrap().rows, rows);
        }
    }
}

#[test]
fn compact_jobs_count_child_activity_only_inside_the_chosen_scope() {
    use aterm_vitals::jobs::Verdict;
    let mut snap = descendant_jobs_scene(Some((84682, 99)));
    let mut view = snap.jobs.as_ref().unwrap().as_ref().clone();
    let mut other = view.rows[0].clone();
    other.tab = 8;
    other.id = "other-tab".into();
    other.working_descendant = Some((999, 100));
    view.rows.push(other);
    view.findings.zombies = vec![
        crate::stats_paint::tests::zombies(901, 2, Some(8)),
        crate::stats_paint::tests::zombies(902, 7, None),
    ];
    snap.set_jobs(Some(Arc::new(view)));
    let m = FaceMetrics::helvetica_neue();
    let inks = dark_inks();
    for (bar, children) in [(Bar::Tab, 1), (Bar::Global, 2)] {
        let sec = jobs_section(Ctx {
            bar,
            panes: &[7],
            ..two_columns(&snap, &m)
        });
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
        (sec.forms.last().unwrap().draw)(
            &mut pen,
            Place {
                x: 0.0,
                y: 0.0,
                w: 526.0,
                h: LINE_H,
            },
        );
        let text = pen
            .boxes
            .iter()
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            text.contains("1 stuck"),
            "the independent stall stays visible: {text}"
        );
        assert!(text.contains(&format!("{children} local stuck")), "{text}");
        assert!(text.contains("child activity"), "{text}");
        assert_eq!(text.contains("2 zombies"), bar == Bar::Global, "{text}");
        assert!(
            !text.contains("7 zombies") && !text.contains("9 zombies"),
            "unassigned zombies remain outside the jobs view: {text}"
        );
    }
    assert_eq!(
        snap.jobs
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .filter(|r| r.verdict == Verdict::Stuck)
            .count(),
        3
    );
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
            "Capacity",
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
    assert_eq!(
        names(Instrument::Network),
        [
            "Now",
            "Traffic now",
            "Throughput, last hour",
            "Open sockets",
            "Connectivity"
        ]
    );
    for focus in [Instrument::Tokens, Instrument::Gpu] {
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
    assert!(details.contains("Codex and agy tabs are not counted"));
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
    assert!(well(&net, "Open sockets").contains("no per-app network figure"));
    let now = well(&net, "Now");
    assert!(!now.is_empty(), "{now}");
}

/// The CPU lens of `snap` on `platform`: every word of its sections.
fn cpu_lens_words(snap: &Snapshot, platform: Platform) -> Vec<String> {
    let m = FaceMetrics::helvetica_neue();
    let c = Ctx {
        loc: Locale { platform, ..LOC },
        ..two_columns(snap, &m)
    };
    cpu_doc(c)
        .sections
        .iter()
        .flat_map(|s| s.words.clone())
        .collect()
}

/// A3 L2 (D2, D3): on Linux the CPU lens prints only lines whose source answered: no IOReport
/// window, CLPC mask, power limits or frequency and residency, whose sources a platform without
/// CPU Stats answers `NO_SOURCE` for (the reader's own sample is that one:
/// `a_platform_without_cpu_stats_reads_no_source`), nor the pointer to them; no ⌘⇧C line, a
/// chord that copies no report on Linux; a heuristic thermal level as an estimate; and its load as
/// load, never less the cores as threads waiting for one. NEGATIVE CONTROL: macOS's lens over a
/// sample whose IOReport subscription was refused, an OS thermal level and the same load prints
/// every one of them.
#[test]
fn the_linux_cpu_lens_prints_no_ioreport_clpc_or_power_limit_words() {
    use aterm_vitals::hardware::Sample;
    use aterm_vitals::model::{Thermal, Throttle};
    let macos_words = [
        "IOReport",
        "CLPC",
        "power limits",
        "frequency/residency",
        "\u{2318}\u{21e7}C copies",
        "Inspect/export",
        "waiting for a core",
        "OS thermal state",
    ];
    let mut snap = lens_scene(Scene::Calm);
    assert_eq!(
        (snap.facts.load1, snap.total_cores()),
        (Some(25.7), Some(18)),
        "PRECONDITION: a load past the cores"
    );
    snap.hardware = Some(Sample::unavailable(aterm_vitals::sys::hardware::NO_SOURCE));
    snap.thermal = Some(Thermal {
        level: Throttle::None,
        raw: 0,
        low_power: None,
        exact: false,
    });
    let linux = cpu_lens_words(&snap, Platform::Linux).join("\n");
    for word in macos_words {
        assert!(!linux.contains(word), "{word}: {linux}");
    }
    for said in [
        "1-minute load 25.7 (counts disk waits too)",
        "thermal estimate: none (from thermal-zone trips; this computer reports no thermal state)",
    ] {
        assert!(linux.contains(said), "{said}: {linux}");
    }
    snap.hardware = Some(Sample::unavailable("refused:ioreport_subscription"));
    snap.thermal = Some(Thermal {
        low_power: Some(false),
        exact: true,
        ..snap.thermal.expect("set above")
    });
    let mac = cpu_lens_words(&snap, Platform::MacOs).join("\n");
    for word in macos_words {
        assert!(mac.contains(word), "CONTROL: {word}: {mac}");
    }
}

/// A3 L2 (D2): the CPU lens says what ⌘⇧C does only on a platform where that chord copies its
/// diagnostic report: where aterm's hardcoded ⌘ (Super) suite is live, never on Linux, whose
/// lens's key handler takes Super+Shift+C as its ⌘C, a copy of its sentence. The handler asks
/// the same question of this build's platform. NEGATIVE CONTROL: the macOS and
/// Windows lenses over the same reading print the key line.
#[test]
fn the_cpu_lens_key_line_prints_only_where_its_key_is_bound() {
    assert_eq!(
        cpu_report_key(Platform::host()),
        crate::app_input::HARDCODED_SUPER_CHORDS
    );
    let mut snap = lens_scene(Scene::Calm);
    snap.hardware = Some(aterm_vitals::hardware::Sample::unavailable(
        "refused:ioreport_subscription",
    ));
    for (platform, bound) in [
        (Platform::MacOs, true),
        (Platform::Windows, true),
        (Platform::Linux, false),
    ] {
        assert_eq!(cpu_report_key(platform), bound, "{platform:?}");
        let said = cpu_lens_words(&snap, platform)
            .iter()
            .any(|w| w.starts_with("\u{2318}\u{21e7}C copies the diagnostic report"));
        assert_eq!(said, bound, "{platform:?}");
    }
}

/// The GPU lens of `snap` on `platform`: its sources, then every word of its sections.
fn gpu_lens_words(snap: &Snapshot, platform: Platform) -> (Vec<String>, String) {
    let m = FaceMetrics::helvetica_neue();
    let c = Ctx {
        loc: Locale { platform, ..LOC },
        ..two_columns(snap, &m)
    };
    let doc = text_well(c, Instrument::Gpu);
    let words = doc
        .sections
        .iter()
        .flat_map(|s| &s.words)
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    (doc.sources, words)
}

/// A3 L4 (D7): the GPU lens says the GPU uses unified system memory where its reader says it
/// works in the system's memory (`GpuSample::unified`), whatever the platform and the name: NVML's
/// GB10 on Linux says it, under NVML's own name. NEGATIVE CONTROL: an Apple-named GPU on macOS
/// whose reading carries no such fact says nothing of it, nor does a discrete card on Linux.
#[test]
fn unified_memory_words_key_on_the_readers_fact_not_the_platform() {
    const UNIFIED: &str = "uses unified system memory";
    let reading = |name: &str, unified: bool| {
        let mut snap = lens_scene(Scene::Calm);
        let gpu = snap.facts.gpu.as_mut().expect("fixture GPU");
        gpu.name = Some(name.into());
        gpu.unified = unified;
        snap
    };
    let (sources, words) = gpu_lens_words(&reading("NVIDIA GB10", true), Platform::Linux);
    assert!(words.contains(UNIFIED), "{words}");
    assert!(
        sources
            .iter()
            .all(|s| s.starts_with("NVIDIA GB10 \u{b7} ") || s.starts_with("as of")),
        "{sources:?}"
    );
    let (_, words) = gpu_lens_words(&reading("Apple M5 Max", true), Platform::MacOs);
    assert!(words.contains(UNIFIED), "{words}");
    for (name, platform) in [
        ("Apple M5 Max", Platform::MacOs),
        ("NVIDIA RTX PRO 6000", Platform::Linux),
    ] {
        let (_, words) = gpu_lens_words(&reading(name, false), platform);
        assert!(
            words.contains("GPU memory total not reported") && !words.contains(UNIFIED),
            "{name} on {platform:?}: {words}"
        );
    }
}

/// A3 L4 (D7, §3.8): only macOS has a reader of the GPU recovery counter, so the Linux GPU lens
/// prints no line of it. NEGATIVE CONTROL: the same reading on macOS says the counter is not read,
/// and a reset the host observed is said on Linux too.
#[test]
fn the_linux_gpu_lens_prints_no_recovery_counter_line() {
    let mut snap = lens_scene(Scene::Calm);
    snap.facts.gpu_resets = None;
    let gpu = snap.facts.gpu.as_mut().expect("fixture GPU");
    gpu.name = Some("NVIDIA GB10".into());
    gpu.recoveries = None;
    let (_, linux) = gpu_lens_words(&snap, Platform::Linux);
    assert!(!linux.contains("recovery counter"), "{linux}");
    let (_, mac) = gpu_lens_words(&snap, Platform::MacOs);
    assert!(mac.contains("GPU recovery counter not read"), "{mac}");
    snap.facts.gpu_resets = Some((2, AT - MIN_MS));
    let (_, reset) = gpu_lens_words(&snap, Platform::Linux);
    assert!(reset.contains("reset 2"), "{reset}");
}

/// The lens's first line says used, available and total (`docs/DESIGN-stats-deck-flow-
/// 2026-11-02.md` §4, "On demand"), which nothing in the deck says now that no row captions its
/// capacity: Memory's Capacity and Disk's Space stay first however short the panel, the Disk's
/// used figure is its size less what this user can still write with the blocks it keeps back
/// said after it where they are 1 % of it or more, and the GPU's source line names the device,
/// its cores and the memory it holds in use. Speech and ⌘C ([`lens_sentence`]) say the same
/// figures. CONTROL: a volume keeping back under 1 % says no reserve, and a GPU whose in-use
/// memory is unread names none.
#[test]
fn the_lens_first_line_says_used_available_and_total() {
    let mut snap = lens_scene(Scene::Calm);
    let m = FaceMetrics::helvetica_neue();
    let memory = snap.mem.as_mut().expect("fixture memory");
    memory.sample.total = 128 * (1_u64 << 30);
    memory.sample.app = 80 * (1_u64 << 30);
    memory.sample.wired = 12 * (1_u64 << 30);
    memory.sample.compressed = Some(4 * (1_u64 << 30));
    memory.sample.cached = 24 * (1_u64 << 30);
    memory.sample.free = 8 * (1_u64 << 30);
    memory.sample.available = 32 * (1_u64 << 30);
    snap.volume = Some(VolumeSample {
        total: 4_000_000_000_000,
        free: 1_660_000_000_000,
        reserved: 40_000_000_000,
        mount: "/".into(),
    });
    let mem_line = "96.0 GiB used \u{b7} 32.0 GiB available \u{b7} 128 GiB total";
    let disk_line = "2.34 TB used \u{b7} 1.66 TB free \u{b7} 4.00 TB total \u{b7} 40 GB reserved";

    let doc = memory_doc(two_columns(&snap, &m));
    assert!(doc.keeps_first, "capacity must survive a short panel");
    let first = &doc.sections[0];
    assert_eq!(first.name, "Capacity");
    assert_eq!(first.words.join(" "), mem_line);
    drop(doc);
    let doc = disk_doc(two_columns(&snap, &m));
    assert!(doc.keeps_first, "space must survive a short panel");
    assert_eq!(doc.sections[0].name, "Space now");
    assert_eq!(doc.sections[0].words[0], disk_line);
    drop(doc);

    let gpu = snap.facts.gpu.as_mut().expect("fixture GPU");
    gpu.name = Some("Apple M5 Max".into());
    gpu.cores = Some(40);
    gpu.in_use_bytes = Some(6 * (1_u64 << 30));
    gpu.alloc_bytes = Some(8 * (1_u64 << 30));
    let doc = text_well(two_columns(&snap, &m), Instrument::Gpu);
    let at = two_columns(&snap, &m).at();
    assert_eq!(
        doc.sources[0],
        format!("Apple M5 Max \u{b7} 40 cores \u{b7} 6.0 GiB in use \u{b7} as of {at}")
    );
    let details = doc
        .sections
        .iter()
        .flat_map(|s| &s.words)
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(details.contains("6.0 GiB GPU memory in use"), "{details}");
    assert!(
        details.contains("8.0 GiB GPU memory allocated"),
        "{details}"
    );
    assert!(
        details.contains("GPU memory total not reported"),
        "{details}"
    );
    assert!(
        !details.contains("128 GiB GPU"),
        "system RAM is not a dedicated VRAM total: {details}"
    );
    drop(doc);

    for (focus, said) in [
        (
            Instrument::Memory,
            "96.0 gibibytes used, 32.0 gibibytes available, 128 gibibytes total",
        ),
        (
            Instrument::Disk,
            "2.34 terabytes used, 1.66 terabytes free, 4.00 terabytes total, 40 gigabytes \
             reserved",
        ),
        (
            Instrument::Gpu,
            "Apple M5 Max, 40 cores, 6.0 gibibytes in use.",
        ),
    ] {
        let spoken = lens_sentence(&snap, focus, &[FRONT], &LOC);
        assert!(spoken.contains(said), "{focus:?}: {spoken}");
    }

    // CONTROL: 39 GB kept back of 4 TB is under 1 %; an unread in-use figure goes unsaid.
    snap.volume.as_mut().expect("volume").reserved = 39_000_000_000;
    let doc = disk_doc(two_columns(&snap, &m));
    assert_eq!(
        doc.sections[0].words[0],
        "2.34 TB used \u{b7} 1.66 TB free \u{b7} 4.00 TB total"
    );
    drop(doc);
    snap.facts.gpu.as_mut().expect("fixture GPU").in_use_bytes = None;
    let doc = text_well(two_columns(&snap, &m), Instrument::Gpu);
    assert_eq!(
        doc.sources[0],
        format!("Apple M5 Max \u{b7} 40 cores \u{b7} as of {at}")
    );
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

/// Why now's sub-heading keeps its words while a live window wobbles about a
/// second (995–1,004 ms from one split tick to the next, §7.2 item 3): the
/// window is rounded to a tenth first and only then given its form. NEGATIVE
/// CONTROL: a window that rounds under 1.0 s keeps its decimal.
#[test]
fn why_nows_window_keeps_its_words_about_a_second() {
    for ms in [995, 1_000, 1_004] {
        let mut snap = engine_1032();
        let why = snap.why.as_mut().expect("the scene's split");
        Arc::make_mut(why).window_ns = ms * 1_000_000;
        let laid = cpu_for(&snap, &[1]);
        let drawn: Vec<&str> = laid.boxes.iter().map(|b| b.text.as_str()).collect();
        assert!(
            drawn.contains(&"1 s window \u{b7} exact, 9 coalitions"),
            "{ms} ms: {drawn:?}"
        );
    }
    for (ms, words) in [
        (949, "0.9 s window"),
        (950, "1 s window"),
        (4_995, "5 s window"),
        (5_004, "5 s window"),
    ] {
        assert_eq!(window_words(ms * 1_000_000), words, "{ms} ms");
    }
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
    let cases: [(fixtures::Scene, &[u64], &[&str]); 10] = [
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
            fixtures::claude_reaped_1032(),
            &[1],
            &[
                "Why: Tab 1's work, 3.20 cores.",
                "this tab is the main cause",
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
        .contains("this computer"),
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
        Instrument::NetworkState
    );
    assert_eq!(
        step_instrument(Instrument::HowToRead, true),
        Instrument::Cpu
    );
    assert_eq!(
        step_instrument(Instrument::HowToRead, false),
        Instrument::NetworkState
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

/// The CPU lens's ⌘⇧P line is drawn whole wherever its section shows it: one row in the 476 pt
/// column of the lens a 1036.5 pt Folded deck opens (988.5 pt wide), wrapped at the narrowest
/// two-column lens (640 pt) and in one column down to the narrowest lens (160 pt), none of its
/// rows cut and no form ending inside it. NEGATIVE CONTROL: the line it replaced is wider than
/// that 476 pt column, which cut it mid-sentence.
#[cfg(target_os = "macos")]
#[test]
fn the_cpu_policy_line_is_drawn_whole_at_every_width() {
    let mut snap = lens_scene(Scene::Calm);
    snap.authorized_cpu_policy = Some(Arc::new(aterm_vitals::sys::json::Jv::obj([(
        "state",
        aterm_vitals::sys::json::Jv::s("ok"),
    )])));
    let m = FaceMetrics::helvetica_neue();
    let input = input(&snap, dark_inks());
    let drawn = |lw: f64| -> (Vec<String>, Vec<String>) {
        let (_, cw) = columns(lw);
        let rows = wrap(&m, CPU_POLICY_KEY, cw, BODY_PT);
        let texts: Vec<String> = lay_lens(
            &shell(lw, 250.0, 2.0),
            &input,
            &pinned(Instrument::Cpu),
            &m,
            false,
        )
        .boxes
        .into_iter()
        .map(|b| b.text)
        .collect();
        (rows, texts)
    };
    for lw in [988.5, 640.0, 639.0, 400.0, 160.0] {
        let (rows, texts) = drawn(lw);
        assert!(
            rows.iter().all(|r| !r.ends_with('\u{2026}')),
            "{lw} pt: {rows:?}"
        );
        assert_eq!(
            rows.concat().replace(' ', ""),
            CPU_POLICY_KEY.replace(' ', ""),
            "{lw} pt"
        );
        let at = texts.iter().position(|t| *t == rows[0]);
        if lw >= 640.0 {
            assert!(at.is_some(), "{lw} pt: shown, {texts:?}");
        }
        if let Some(at) = at {
            assert_eq!(
                texts.get(at..at + rows.len()),
                Some(&rows[..]),
                "{lw} pt: whole"
            );
        }
        assert!(
            !texts
                .iter()
                .any(|t| t.contains("\u{2318}\u{21e7}P") && t.ends_with('\u{2026}')),
            "{lw} pt: {texts:?}"
        );
    }
    assert_eq!(
        drawn(988.5).0.len(),
        1,
        "one row at the 1036.5 pt Folded deck"
    );
    let replaced = "\u{2318}\u{21e7}P authorizes one bounded, read-only CPU policy collection in \
                    aterm's password sheet. No policy changes or credential cache.";
    assert!(
        m.width(replaced, BODY_PT, false) > columns(988.5).1,
        "CONTROL"
    );
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
        lens_body(columns(526.0), 0.0, bottom, &sections, (&[], false))
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
        lens_body(columns(526.0), 0.0, bottom, &kept, (&[], keeps))
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
fn lens_gallery() {
    let (dir, _gallery) = crate::scripted_redraw_fixture::gallery_directory("ATERM_LENS_GALLERY");
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let calm = lens_scene(Scene::Calm);
    let trouble = lens_scene(Scene::Trouble);
    let descendant = descendant_jobs_scene(Some((84682, 1_793_674_154_348_639)));
    let no_descendant = descendant_jobs_scene(None);
    let mut net_live = lens_scene(Scene::Calm);
    let mut net_rng = Rng(11);
    net_live.resources[Resource::Network.index()].capacity = Some(125_000_000.0);
    net_live.resources[Resource::Network.index()].now = Some(8_400_000.0);
    net_live.resources[Resource::Network.index()].heat_now = Some(0.067);
    net_live.resources[Resource::Network.index()].ring = hour_ring(
        &net_live,
        Resource::Network.index(),
        0.08,
        0.03,
        &mut net_rng,
    );
    net_live.resources[Resource::Network.index()].aterm = ScopeFig {
        now: Some(12.0),
        missing: None,
        ..ScopeFig::default()
    };
    net_live.facts.link = Some(aterm_vitals::LinkSample {
        name: "en0".into(),
        kind: aterm_vitals::LinkKind::Wifi,
        rate_bps: Some(1_000_000_000),
        ..aterm_vitals::LinkSample::default()
    });
    net_live.net = Some(aterm_vitals::NetView {
        rates: Some(aterm_vitals::LinkRates {
            rx_bps: 8_400_000.0,
            tx_bps: 1_200_000.0,
            rx_pps: 6_000.0,
            tx_pps: 1_000.0,
            rx_err_ps: 0.0,
            tx_err_ps: 0.0,
            rx_drop_ps: 0.0,
            tx_drop_ps: None,
        }),
        tcp: Some(aterm_vitals::TcpCounts {
            established: 15,
            listen: 11,
            time_wait: 1,
            ..aterm_vitals::TcpCounts::default()
        }),
        srtt_ms: Some(14.6),
        state: aterm_vitals::NetState::Online,
        aterm_sockets: Some(12),
        tab_sockets: vec![(FRONT, 4), (3, 7)],
        ..aterm_vitals::NetView::default()
    });
    let mut shots: Vec<(String, &Snapshot, Instrument, f64, f64)> = Vec::new();
    for (name, snap) in [
        ("child_activity", &descendant),
        ("no_child_activity", &no_descendant),
    ] {
        for width in [1024.0, 526.0] {
            shots.push((
                format!("jobs_{name}_{width}x250"),
                snap,
                Instrument::Processes,
                width,
                250.0,
            ));
        }
    }
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
    shots.push((
        "network_1024x250".into(),
        &net_live,
        Instrument::Network,
        1024.0,
        250.0,
    ));
    shots.push((
        "network_526x250".into(),
        &net_live,
        Instrument::Network,
        526.0,
        250.0,
    ));
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
    assert_eq!(written.len(), (6 + 12 + 3 + 4) * 2);
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

#[test]
fn cpu_and_network_lenses_surface_chip_connectivity_latency_and_scoped_sockets() {
    let mut snap = lens_scene(Scene::Calm);
    snap.facts.cpu_brand = Some("Apple M5 Max".into());
    if let Some(cpu) = snap.cpu.as_mut() {
        cpu.core_history =
            vec![vec![Some(0.95); aterm_vitals::model::CORE_HISTORY_LEN]; cpu.total()];
    }
    let inks = dark_inks();
    let cpu_laid = lay_lens(
        &shell(1119.0, 760.0, 2.0),
        &input(&snap, inks),
        &pinned(Instrument::Cpu),
        &FaceMetrics::helvetica_neue(),
        true,
    );
    assert!(
        cpu_laid
            .boxes
            .iter()
            .any(|b| b.text.contains("Apple M5 Max")),
        "CPU lens header names the processor chip: {:?}",
        cpu_laid.boxes
    );
    let hot_core = rgba(inks.heat[usize::from(ink::heat_step(0.95))], 0xFF);
    assert!(
        cpu_laid
            .prims
            .iter()
            .any(|p| matches!(p, DrawPrim::Panel { fill, .. } if *fill == hot_core)),
        "CPU lens colors hot core history cells in the deck's heat ramp"
    );

    snap.facts.link = Some(aterm_vitals::LinkSample {
        name: "en0".into(),
        kind: aterm_vitals::LinkKind::Wifi,
        rate_bps: Some(1_000_000_000),
        ..aterm_vitals::LinkSample::default()
    });
    snap.net = Some(aterm_vitals::NetView {
        rates: Some(aterm_vitals::LinkRates {
            rx_bps: 8_400_000.0,
            tx_bps: 1_200_000.0,
            rx_pps: 6_000.0,
            tx_pps: 1_000.0,
            rx_err_ps: 0.0,
            tx_err_ps: 0.0,
            rx_drop_ps: 0.0,
            tx_drop_ps: None,
        }),
        tcp: Some(aterm_vitals::TcpCounts {
            established: 15,
            listen: 11,
            time_wait: 1,
            ..aterm_vitals::TcpCounts::default()
        }),
        srtt_ms: Some(14.6),
        state: aterm_vitals::NetState::Online,
        aterm_sockets: Some(12),
        tab_sockets: vec![(FRONT, 4), (3, 7)],
        ..aterm_vitals::NetView::default()
    });
    snap.resources[Resource::Network.index()].aterm = aterm_vitals::ScopeFig {
        now: Some(12.0),
        missing: None,
        ..aterm_vitals::ScopeFig::default()
    };
    let net = heard(&snap, pinned(Instrument::Network));
    let traffic = well(&net, "Traffic now");
    assert!(
        traffic.contains("download") && traffic.contains("8.4 MB/s") && traffic.contains("upload"),
        "Network lens splits download and upload rates: {traffic}"
    );
    let conn = well(&net, "Connectivity");
    assert!(
        conn.contains("Online") && conn.contains("median TCP RTT 14.6 ms"),
        "Network lens surfaces live state and passive TCP RTT: {conn}"
    );
    let sockets = well(&net, "Open sockets");
    assert!(
        sockets.contains("aterm, 12 sockets") && sockets.contains("Tab 2, 4 sockets"),
        "Network lens lists machine, aterm, and tab socket counts: {sockets}"
    );
    assert!(
        !sockets.contains("no per-app network figure"),
        "Network lens drops 'no per-app network figure' when socket figures are present: {sockets}"
    );
    let tcp = well(&net, "TCP states");
    assert!(
        tcp.contains("15 established") && tcp.contains("11 listen") && tcp.contains("1 time-wait"),
        "Network lens summarizes TCP states: {tcp}"
    );
    let mut counters_only = snap.clone();
    if let Some(net) = counters_only.net.as_mut() {
        net.tcp = Some(aterm_vitals::TcpCounts {
            established: 15,
            time_wait: 1,
            counters_only: true,
            ..aterm_vitals::TcpCounts::default()
        });
    }
    let unread = heard(&counters_only, pinned(Instrument::Network));
    let states = well(&unread, "TCP states");
    assert!(
        states.contains("15 established or close-wait") && !states.contains("listen"),
        "with only the counters read, the lens says no listen count, not 0: {states}"
    );
    assert!(
        !well(&unread, "Open sockets").contains("16 open"),
        "nor a machine total from two states"
    );
    if let Some(net) = counters_only.net.as_mut() {
        net.aterm_sockets = None;
        net.tab_sockets.clear();
    }
    let unread = heard(&counters_only, pinned(Instrument::Network));
    let sockets = well(&unread, "Open sockets");
    assert!(
        sockets.contains("open sockets not read") && !sockets.contains("none open"),
        "no dump: the socket scopes are unread, not none open: {sockets}"
    );
    let net_drawn = lay_lens(
        &shell(1024.0, 250.0, 2.0),
        &input(&snap, dark_inks()),
        &pinned(Instrument::Network),
        &FaceMetrics::helvetica_neue(),
        true,
    );
    assert!(
        faults(&net_drawn, 1024.0, 250.0).is_empty(),
        "{:?}",
        faults(&net_drawn, 1024.0, 250.0)
    );
    assert!(
        net_drawn
            .boxes
            .iter()
            .any(|b| b.text.contains("open sockets counted per scope")),
        "Network lens draws the per-scope socket attribution note: {:?}",
        net_drawn.boxes
    );
    assert!(
        net_drawn
            .boxes
            .iter()
            .any(|b| b.text.contains("link 125 MB/s"))
            && !net_drawn
                .boxes
                .iter()
                .any(|b| b.text.contains("1,000 MB/s")),
        "Network lens converts 1 Gbps link_bps (bits/s) to 125 MB/s (bytes/s): {:?}",
        net_drawn.boxes
    );
    let tab_lens = Lens {
        focus: Instrument::Network,
        bar: Bar::Tab,
        how: Opened::Pinned,
    };
    let net_tab = lay_lens(
        &shell(1024.0, 250.0, 2.0),
        &input(&snap, dark_inks()),
        &tab_lens,
        &FaceMetrics::helvetica_neue(),
        false,
    );
    let tab_sockets = well(&net_tab, "Open sockets in Tab 2");
    assert!(
        tab_sockets.contains("Tab 2, 4 sockets") && !tab_sockets.contains("aterm,"),
        "Bar::Tab filters Open sockets to the active tab: {tab_sockets}"
    );
}

#[test]
fn how_to_read_and_traces_surface_multiscale_gaps_and_events() {
    let words_said = how_to_read_words(Platform::MacOs);
    assert!(
        words_said.contains("4 hours")
            && words_said.contains("6 hours")
            && words_said.contains("1 second, 10 seconds, 1 minute, 10 minutes and 1 hour")
            && words_said.contains("live now cell at the left end")
            && words_said.contains("on a narrow deck the line under each percent figure")
            && !words_said.contains("at the right")
            && !words_said.contains("two and a half")
            && !words_said.contains("15 minutes"),
        "how_to_read_words reflects multi-scale live now strip: {words_said}"
    );
    let mut snap = lens_scene(Scene::Calm);
    let htr = heard(&snap, pinned(Instrument::HowToRead));
    assert!(
        htr.explainer
            .contains("up to 6 h where a strip has the room")
            && htr.explainer.contains("1 s, 10 s, 1 min, 10 min and 1 h"),
        "How to read explainer reflects multi-scale horizons: {}",
        htr.explainer
    );
    let cells = well(&htr, "Cells");
    assert!(
        cells.contains("live now") && cells.contains("sleep, crash"),
        "Cells section explains multi-scale live now and gaps/events: {cells}"
    );
    // Every form the Cells legend draws reads from now, never toward it: a short lens's one line
    // once said "multi-scale to live now", the old axis's direction (the review of 2026-10-26).
    let m = FaceMetrics::helvetica_neue();
    let mut drawn = std::collections::BTreeSet::new();
    for lw in [1024.0, 760.0, 526.0, 420.0, 340.0] {
        for lh in [90.0, 120.0, 160.0, 250.0, 400.0] {
            let laid = lay_lens(
                &shell(lw, lh, 2.0),
                &input(&snap, dark_inks()),
                &pinned(Instrument::HowToRead),
                &m,
                true,
            );
            drawn.extend(
                laid.boxes
                    .iter()
                    .filter(|b| b.text.starts_with("multi-scale"))
                    .map(|b| b.text.clone()),
            );
        }
    }
    assert!(
        drawn.iter().any(|t| t.contains('\u{b7}')),
        "CONTROL: a lens draws the Cells legend's one line: {drawn:?}"
    );
    assert!(
        drawn
            .iter()
            .all(|t| !t.contains("to live now") && !t.contains("to now")),
        "the Cells legend reads from now: {drawn:?}"
    );

    // Populate a crash gap across the unread span in Memory's 1-hour ring (buckets 120..132 of 360
    // 10s buckets = minutes 20..22 of the 60-minute window) and wake/pressure event markers.
    let now = snap.at_unix_ms;
    let hour_start = (now.div_euclid(60_000) - 59) * 60_000;
    snap.time_gaps = vec![
        aterm_vitals::TimeGap {
            start_ms: hour_start + 19 * 60_000,
            end_ms: hour_start + 23 * 60_000,
            kind: aterm_vitals::GapKind::Crash,
        },
        aterm_vitals::TimeGap {
            start_ms: now - 12 * 3_600_000,
            end_ms: now - 10 * 3_600_000,
            kind: aterm_vitals::GapKind::Sleep,
        },
    ];
    snap.strip_events = vec![
        aterm_vitals::StripEvent {
            at_unix_ms: hour_start + 23 * 60_000,
            kind: aterm_vitals::StripEventKind::Crash,
            label: "unclean exit".into(),
        },
        aterm_vitals::StripEvent {
            at_unix_ms: hour_start + 44 * 60_000,
            kind: aterm_vitals::StripEventKind::PressureSpike,
            label: "memory pressure critical".into(),
        },
        aterm_vitals::StripEvent {
            at_unix_ms: now - 10 * 3_600_000,
            kind: aterm_vitals::StripEventKind::SleepWake,
            label: "woke from sleep".into(),
        },
    ];
    let m = FaceMetrics::helvetica_neue();
    for (focus, expect_gap, expect_ev) in [
        (Instrument::Memory, "crash", "pressure"),
        (Instrument::Disk, "asleep", "woke"),
    ] {
        for lw in [1024.0, 526.0] {
            let laid = lay_lens(
                &shell(lw, 250.0, 2.0),
                &input(&snap, dark_inks()),
                &pinned(focus),
                &m,
                true,
            );
            let why = faults(&laid, lw, 250.0);
            assert!(why.is_empty(), "{focus:?} {lw}: {why:?}");
            if lw >= 1024.0 {
                assert!(
                    laid.boxes.iter().any(|b| b.text == expect_gap),
                    "{focus:?}: expected gap label {expect_gap:?} in {:?}",
                    laid.boxes
                );
                assert!(
                    laid.boxes.iter().any(|b| b.text == expect_ev),
                    "{focus:?}: expected event label {expect_ev:?} in {:?}",
                    laid.boxes
                );
                if focus == Instrument::Memory {
                    assert_eq!(
                        laid.boxes.iter().filter(|b| b.text == "crash").count(),
                        1,
                        "narrow crash gap keeps the top event label without colliding a second bottom label across the vertical rule: {:?}",
                        laid.boxes
                    );
                }
            }
        }
    }
}

/// A lens trace keeps its own axis and its uniform time (`docs/DESIGN-stats-deck-units-2026-11-04.md`
/// §7): the axis spans every mean and peak and labels as [`axis`] lays it, each of the 60 columns
/// stands at `x0 + j · pitch`, and `skyline_box` draws it at its axis height, its tick at its
/// peak's, both in the heat its lens names (not the heat of its height), its cap at its over
/// share. An unread column draws no box.
#[test]
fn lens_traces_keep_their_axes_and_time_base() {
    let inks = dark_inks();
    let m = FaceMetrics::helvetica_neue();
    let n = 60;
    let vals: Vec<Option<f64>> = (0..n)
        .map(|j| (!(20..24).contains(&j)).then_some(10.0 + 0.1 * j as f64))
        .collect();
    let peaks: Vec<Option<f64>> = vals.iter().map(|v| v.map(|v| v + 1.5)).collect();
    let heat: Vec<HeatCell> = (0..n)
        .map(|j| HeatCell {
            heat: Some(0.08),
            max: Some(0.45),
            over: if j == 40 { 0.5 } else { 0.0 },
            gap: None,
        })
        .collect();
    let trace = Trace {
        vals: vals.clone(),
        peaks: peaks.clone(),
        heat,
        under: HOUR,
        min_span: 0.5,
        mark: None,
        gaps: Vec::new(),
        events: Vec::new(),
    };
    for scale in [1.0_f32, 2.0] {
        let mut pen = Pen {
            p: Painter::new(&inks, scale, 14.0 * scale, false),
            m: &m,
            inks,
            ic: false,
            s: f64::from(scale),
            ox: 0.0,
            oy: 0.0,
            draw: true,
            line_tab: 0.0,
            boxes: Vec::new(),
        };
        let p = Place {
            x: 12.0,
            y: 30.0,
            w: 420.0,
            h: 120.0,
        };
        draw_trace(&mut pen, p, &trace);
        let s = f64::from(scale);
        let px = |v: f64| (v * s + 0.5).floor() as f32;
        // The axis: over the means and the peaks, as `axis` lays it.
        let ch = p.h - 32.0;
        let read: Vec<f64> = vals.iter().chain(&peaks).flatten().copied().collect();
        let (mn, mx) = read
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &v| {
                (a.min(v), b.max(v))
            });
        let k = if ch >= 60.0 { 3 } else { 2 };
        let (lo, step) = axis(mn, mx, k, trace.min_span);
        let hi = lo + step * k as f64;
        assert!(
            hi >= mx && lo <= mn,
            "CONTROL: the axis spans the peaks ({lo}..{hi})"
        );
        let dec = decimals(step).max(decimals(lo));
        let labels: Vec<String> = (0..=k)
            .map(|i| format!("{:.*}", dec, lo + step * i as f64))
            .collect();
        for l in &labels {
            assert!(
                pen.boxes.iter().any(|b| &b.text == l),
                "{scale}×: the axis label {l:?} in {:?}",
                pen.boxes.iter().map(|b| &b.text).collect::<Vec<_>>()
            );
        }
        let gutter = labels
            .iter()
            .map(|l| m.width(l, AXIS_PT, false))
            .fold(0.0, f64::max)
            + 4.0;
        let (x0, x1) = (p.x + gutter, p.x + p.w);
        let (y0, y1) = (p.y + 18.0, p.y + 18.0 + ch);
        let pitch = (x1 - x0) / n as f64;
        let (top, bottom) = (px(y0), px(y1));
        let h = bottom - top;
        let track = rgba(inks.track, 0xFF);
        let plates: Vec<(f32, f32)> = pen
            .p
            .prims
            .iter()
            .filter_map(|q| match *q {
                DrawPrim::Panel {
                    x,
                    y,
                    w,
                    h: ph,
                    fill,
                    ..
                } if fill == track && y == top && ph == h => Some((x, w)),
                _ => None,
            })
            .collect();
        let read_cols: Vec<usize> = (0..n).filter(|&j| vals[j].is_some()).collect();
        assert_eq!(
            plates.len(),
            read_cols.len(),
            "{scale}×: one plate a read column"
        );
        let gap = if pitch * s >= 4.0 { 1.0 } else { 0.0 };
        let share = |v: f64| ((v - lo) / (hi - lo)) as f32;
        for (&j, &(x, w)) in read_cols.iter().zip(&plates) {
            let at = format!("{scale}×, column {j}");
            let xa = px(x0 + j as f64 * pitch);
            assert_eq!(x, xa, "{at}: uniform time");
            assert_eq!(w, px(x0 + (j + 1) as f64 * pitch) - gap - xa, "{at}");
            let v = vals[j].expect("read");
            let want_h = (share(v) * h).round().clamp(1.0, h);
            let column = pen.p.prims.iter().find_map(|q| match *q {
                DrawPrim::Panel {
                    x: cx,
                    y,
                    w: cw,
                    h: chh,
                    fill,
                    ..
                } if cx == x && cw == w && y + chh == bottom && fill != track => Some((chh, fill)),
                _ => None,
            });
            assert_eq!(
                column,
                Some((want_h, rgba(inks.cell_at(0.08, false), 0xFF))),
                "{at}: the column at its axis height in its lens's heat"
            );
            let peak_h = (share(peaks[j].expect("read")) * h).round().clamp(1.0, h);
            let tick = pen.p.prims.iter().any(|q| {
                matches!(*q, DrawPrim::Panel { y, h: th, fill, .. }
                    if y == bottom - peak_h && th == 1.0 && fill == rgba(inks.cell_at(0.45, false), 0xFF))
            });
            assert_eq!(tick, peak_h >= want_h + 2.0, "{at}: the peak tick");
        }
        let over = rgba(inks.over, 0xFF);
        let caps: Vec<f32> = pen
            .p
            .prims
            .iter()
            .filter_map(|q| match *q {
                DrawPrim::Panel { x, y, fill, .. } if fill == over && y < top => Some(x),
                _ => None,
            })
            .collect();
        assert_eq!(caps.len(), 1, "{scale}×: column 40's cap alone");
        let (cx0, cx1) = (px(x0 + 40.0 * pitch), px(x0 + 41.0 * pitch));
        assert!(
            (cx0..cx1).contains(&caps[0]),
            "{scale}×: the cap over its column"
        );
        assert!(
            !pen.p
                .prims
                .iter()
                .any(|q| matches!(q, DrawPrim::Line { .. })),
            "{scale}×: no step line is left"
        );
    }
}
