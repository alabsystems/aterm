// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The merged deck's skeleton over the mockups' scenes ([`aterm_vitals::fixtures`]): its words
//! stay inside the card and their rooms and never meet, each block names its instrument in each
//! lane, the reader meets the needs first, and a snapshot lays the same pixels every time.

use aterm_vitals::fixtures::{self, DeckScene};
use aterm_vitals::ink::VitalsInks;
use aterm_vitals::model::{CapBasis, ResourceView};

use super::*;
use crate::merged_plan::{Rule, row_a_px};
use crate::stats_deck::deck_fixtures::DeckContext;
use crate::stats_deck::prims_fp;
use crate::stats_paint::HitKind;
use crate::stats_plan::DeckForm;

/// The widths the gallery and the mockups draw at, in points, and their scales.
const SIZES: [(f64, f64); 4] = [(1119.0, 2.0), (550.0, 2.0), (350.0, 2.0), (1119.0, 1.0)];
/// The three-row deck's least, the mockups' and its greatest height, in points.
const DECKS_PT: [f64; 3] = [45.0, 51.0, 60.0];
const TOL: f64 = 0.01;

fn dark() -> VitalsInks {
    crate::stats_deck::tests::dark_inks()
}

/// How tall a card is: a three-row deck of so many points, or row A alone under the least.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Height {
    Three(f64),
    RowA,
}

impl Height {
    /// The card's height and the three-row deck it is laid as, in device px at `s`.
    fn px(self, s: f64) -> (usize, usize) {
        let least = (MERGED_MIN_PT * s).round() as usize;
        match self {
            Self::Three(pt) => {
                let deck = (pt * s).round() as usize;
                (deck, deck)
            }
            Self::RowA => (row_a_px(least), least),
        }
    }

    /// Every height the sweep lays: each three-row deck, then row A alone.
    fn all() -> [Self; 4] {
        [
            Self::Three(DECKS_PT[0]),
            Self::Three(DECKS_PT[1]),
            Self::Three(DECKS_PT[2]),
            Self::RowA,
        ]
    }
}

fn geom(w: f64, s: f64, h: Height) -> DeckGeom {
    DeckGeom {
        w: (w * s) as f32,
        h: h.px(s).0 as f32,
        pad: (12.0 * s) as f32,
        scale: s as f32,
        font_px: (13.0 * s) as f32,
    }
}

fn input_of(scene: DeckScene, snap: &Snapshot, inks: VitalsInks) -> DeckInput<'_> {
    DeckContext::of(scene).input(Some(snap), DeckForm::Full, inks)
}

fn planner(snap: &Snapshot, s: f64, h: Height, m: &FaceMetrics) -> MergedPlanner {
    let shape = MachineShape::of(Some(snap), Platform::MacOs);
    MergedPlanner::new(h.px(s).1, s, shape, m)
}

fn lay(scene: DeckScene, w: f64, s: f64, h: Height, m: &FaceMetrics) -> MergedLaid {
    lay_snap(scene, &fixtures::snapshot(scene), w, s, h, m)
}

/// [`lay`] with `snap` in the place of the scene's own snapshot.
fn lay_snap(
    scene: DeckScene,
    snap: &Snapshot,
    w: f64,
    s: f64,
    h: Height,
    m: &FaceMetrics,
) -> MergedLaid {
    let input = input_of(scene, snap, dark());
    merged_lay(&planner(snap, s, h, m), &geom(w, s, h), &input, m, true)
}

fn texts(laid: &MergedLaid) -> Vec<(String, Rgb, TextFace)> {
    laid.prims
        .iter()
        .filter_map(|p| match p {
            DrawPrim::Text { s, color, face, .. } => {
                Some((s.clone(), [color[0], color[1], color[2]], *face))
            }
            _ => None,
        })
        .collect()
}

fn overlap(a: &[f64; 4], b: &[f64; 4]) -> bool {
    a[0] < b[2] - TOL && b[0] < a[2] - TOL && a[1] < b[3] - TOL && b[1] < a[3] - TOL
}

/// What is wrong with one laid card `w` × `h` pt: a box past the card or its room, two boxes
/// that meet, a block of the form whose name is not drawn, or a band whose name is not.
fn faults(laid: &MergedLaid, snap: &Snapshot, w: f64, h: f64) -> Vec<String> {
    let mut why = Vec::new();
    let boxes = &laid.boxes;
    for b in boxes {
        if b.r[0] < -TOL || b.r[1] < -TOL || b.r[2] > w + TOL || b.r[3] > h + TOL {
            why.push(format!(
                "{:?} {:?} {:?} leaves the card",
                b.kind, b.text, b.r
            ));
        }
        if let Some((x0, x1)) = b.room
            && (b.r[0] < x0 - TOL || b.r[2] > x1 + TOL)
        {
            why.push(format!("{:?} {:?} leaves its room {x0}..{x1}", b.text, b.r));
        }
    }
    let mut by_x: Vec<&Boxed> = boxes.iter().collect();
    by_x.sort_by(|a, b| a.r[0].total_cmp(&b.r[0]));
    for (i, a) in by_x.iter().enumerate() {
        for b in by_x.iter().skip(i + 1) {
            if b.r[0] >= a.r[2] - TOL {
                break;
            }
            if overlap(&a.r, &b.r) {
                why.push(format!(
                    "{:?} {:?} {:?} meets {:?} {:?} {:?}",
                    a.kind, a.text, a.r, b.kind, b.text, b.r
                ));
            }
        }
    }
    let drawn = |s: &str| boxes.iter().any(|b| b.kind == BoxKind::Text && b.text == s);
    for b in &laid.plan.blocks {
        let (name, _) = head_words(b.kind, laid.plan.laid, Some(snap));
        if !drawn(name) {
            why.push(format!(
                "CONTROL: {:?}'s name {name:?} is not drawn",
                b.kind
            ));
        }
    }
    if laid.plan.lanes.scopes.is_some() {
        let bands: &[DeckScope] = match laid.plan.laid {
            MergedForm::Full | MergedForm::Mid => &DeckScope::ALL,
            MergedForm::Narrow => &[DeckScope::AllTabs, DeckScope::ThisTab],
            MergedForm::Rail | MergedForm::RowA => &[],
        };
        for scope in bands {
            if !drawn(scope.word(Platform::MacOs)) {
                why.push(format!("CONTROL: {scope:?}'s band is not named"));
            }
        }
    }
    why
}

/// Every scene at every width from 160 to 5120 pt, in Helvetica Neue and the test stack, at 1×
/// and 2×, as a three-row deck of 45, 51 and 60 pt and as row A alone: no word or mark leaves the
/// card or its room, none meets another, and every block and band of the form is named.
#[test]
fn no_text_overlaps_and_none_leaves_the_card() {
    let faces = [
        ("Helvetica Neue", FaceMetrics::helvetica_neue()),
        ("the test stack", FaceMetrics::test_stack()),
    ];
    let snaps: Vec<(DeckScene, Snapshot)> = DeckScene::ALL
        .into_iter()
        .map(|sc| (sc, fixtures::snapshot(sc)))
        .collect();
    let mut jobs = Vec::new();
    for (scene, snap) in &snaps {
        for (face, m) in &faces {
            for s in [1.0, 2.0] {
                for h in Height::all() {
                    jobs.push((*scene, snap, *face, m, s, h));
                }
            }
        }
    }
    let failed: Vec<String> = std::thread::scope(|scope| {
        let running: Vec<_> = jobs
            .into_iter()
            .map(|(scene, snap, face, m, s, h)| {
                scope.spawn(move || {
                    let input = input_of(scene, snap, dark());
                    let planner = planner(snap, s, h, m);
                    let card_h = h.px(s).0 as f64 / s;
                    let mut failed = Vec::new();
                    let mut forms = Vec::new();
                    for w in (160..=5120).map(f64::from) {
                        let laid = merged_lay(&planner, &geom(w, s, h), &input, m, false);
                        if !forms.contains(&laid.plan.laid) {
                            forms.push(laid.plan.laid);
                        }
                        for why in faults(&laid, snap, w, card_h) {
                            if failed.len() < 20 {
                                failed
                                    .push(format!("{scene:?}, {face}, {s}×, {h:?}, {w} pt: {why}"));
                            }
                        }
                    }
                    if forms.len() < MergedForm::LADDER.len() {
                        failed.push(format!(
                            "CONTROL: {scene:?}, {face}, {s}×, {h:?} laid only {forms:?}"
                        ));
                    }
                    failed
                })
            })
            .collect();
        running
            .into_iter()
            .flat_map(|run| {
                run.join()
                    .unwrap_or_else(|_| vec!["a sweep panicked".to_owned()])
            })
            .collect()
    });
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// Row A names each block of the form in bold primary ink and its capacity, where read, after
/// it in secondary ink, then what is past capacity in orange; the bands are named once in their
/// own inks; then the CPU's kind letters and figure, and its run's ends where it has a run, and
/// Network's figure and its run's ends; an absent reading writes nothing (§4: an absence is
/// drawn, not written).
#[test]
fn row_a_names_each_block_and_its_capacity_where_read() {
    let bold = |s: &str, c| (s.to_owned(), c, TextFace::UiBold);
    let plain = |s: &str, c| (s.to_owned(), c, TextFace::Ui);
    let m = FaceMetrics::helvetica_neue();
    let busy = fixtures::snapshot(DeckScene::Busy);
    let ink = dark();
    let (words, secondary) = (ink.words, ink.merged.secondary);
    let (teal, violet) = (ink.merged.all_tabs, ink.merged.this_tab);
    let (orange, axis) = (ink.merged.over_text, ink.merged.axis);
    let waiting = plain("~8 waiting", orange);
    let cpu = [
        bold("S", secondary),
        bold("P", secondary),
        bold("99%", words),
    ];
    let ends = [plain("10m", axis), plain("now", axis)];
    let capacity = [
        words::cpu_capacity(busy.cpu.as_ref().expect("busy reads its CPU")),
        words::memory_capacity(busy.mem.as_ref().expect("and memory").sample.total),
        words::gpu_capacity(busy.gpu.as_ref().and_then(|g| g.cores).expect("GPU cores")),
        words::disk_capacity(busy.volume.as_ref().expect("and its volume").total),
        "~1 Gb/s".to_owned(),
    ];
    let bands = [
        bold("Mac", words),
        bold("all tabs", teal),
        bold("this tab", violet),
    ];
    let full = |caps: Option<&[String; 5]>| {
        let cap = |i: usize| caps.map(|c| plain(&c[i], secondary));
        let mut want = vec![
            bold("Processes", words),
            plain("CPU, GiB memory", secondary),
        ];
        want.push(bold("CPU", words));
        want.extend(cap(0));
        want.extend(caps.map(|_| waiting.clone()));
        want.push(bold("Memory", words));
        want.extend(cap(1));
        want.push(bold("GPU", words));
        want.extend(cap(2));
        want.push(bold("Disk", words));
        want.extend(cap(3));
        want.push(bold("Network", words));
        want.extend(cap(4));
        want.extend(bands.iter().cloned());
        want
    };
    let mut busy_full = full(Some(&capacity));
    let row_a_words = busy_full.len() - bands.len();
    busy_full.extend(cpu.iter().cloned().chain(ends.iter().cloned()));
    // Network's figure, then its run's ends beside the run, in the Mac lane.
    busy_full.push(bold("12%", words));
    busy_full.extend(ends.iter().cloned());
    assert_eq!(
        texts(&lay(DeckScene::Busy, 1119.0, 2.0, Height::Three(51.0), &m)),
        busy_full,
        "busy, Full"
    );
    let mut absent = full(None);
    absent.extend(ends.iter().chain(&ends).cloned());
    assert_eq!(
        texts(&lay(
            DeckScene::Absent,
            1119.0,
            2.0,
            Height::Three(51.0),
            &m
        )),
        absent,
        "absent, Full: names, no capacity and no figure, and the hollow runs' ends"
    );
    let mut mid = vec![
        bold("Top procs", words),
        plain("· GiB mem", secondary),
        bold("CPU", words),
        waiting.clone(),
        bold("Mem", words),
        bold("Disk", words),
    ];
    mid.extend(bands.iter().cloned());
    mid.extend(cpu.iter().cloned());
    assert_eq!(
        texts(&lay(DeckScene::Busy, 550.0, 2.0, Height::Three(51.0), &m)),
        mid,
        "busy, Mid"
    );
    let mut narrow = vec![
        bold("CPU", words),
        waiting.clone(),
        bold("Mem", words),
        bold("GiB = mem", words),
        bold("all tabs", teal),
        bold("this tab", violet),
    ];
    // At 2× the narrow deck's tiles would be thinner than two device px, so its parts are one
    // tank across both kinds, which needs no letter.
    narrow.push(cpu[2].clone());
    assert_eq!(
        texts(&lay(DeckScene::Busy, 350.0, 2.0, Height::Three(51.0), &m)),
        narrow,
        "busy, Narrow"
    );
    let row_a = lay(DeckScene::Busy, 1119.0, 2.0, Height::RowA, &m);
    assert_eq!(
        texts(&row_a),
        busy_full[..row_a_words].to_vec(),
        "row A alone keeps row A's words and names no band"
    );
}

/// Busy's snapshot with Network's row changed by `f`.
fn busy_network(f: impl FnOnce(&mut ResourceView)) -> Snapshot {
    let mut snap = fixtures::snapshot(DeckScene::Busy);
    f(&mut snap.resources[Resource::Network.index()]);
    snap
}

/// The widths Full lays Network at, with their scales.
const NETWORK_SIZES: [(f64, f64); 2] = [(1119.0, 2.0), (1300.0, 1.0)];

/// Network's header names what 100% is (§9): the reference link's driver rate under the link
/// basis, the busier direction's learned peak under the peak basis (its figure that direction's
/// share), and nothing while it learns, when its gauge is an absence and it writes no figure.
#[test]
fn network_names_the_rate_its_basis_holds_as_100_percent() {
    let m = FaceMetrics::helvetica_neue();
    let net_words = |laid: &MergedLaid| -> Vec<String> {
        let x = laid
            .plan
            .block(BlockKind::Network)
            .expect("Full lays Network")
            .at
            .x;
        let x = (x * laid.plan.scale + 0.5).floor() as f32 - 1.0;
        laid.prims
            .iter()
            .filter_map(|p| match p {
                DrawPrim::Text { x: tx, s, .. } if *tx >= x => Some(s.clone()),
                _ => None,
            })
            .collect()
    };
    let link = lay(DeckScene::Busy, 1119.0, 2.0, Height::Three(51.0), &m);
    assert_eq!(
        net_words(&link),
        ["Network", "~1 Gb/s", "12%", "10m", "now"],
        "the link basis: 1 Gb/s"
    );
    // The learned peaks: 60 MB/s down, of which 15 are in use (25%), and 5 MB/s up, of which 3
    // (60%): the upload binds, so its peak names 100%.
    let peak = busy_network(|v| {
        v.basis = CapBasis::Peak;
        v.now = Some(15e6);
        v.capacity = Some(60e6);
        let up = v.second.as_mut().expect("Network reads both directions");
        (up.now, up.capacity, up.binds) = (Some(3e6), Some(5e6), true);
        v.heat_now = Some(0.6);
    });
    let laid = lay_snap(DeckScene::Busy, &peak, 1119.0, 2.0, Height::Three(51.0), &m);
    assert_eq!(
        net_words(&laid),
        ["Network", "~40 Mb/s", "60%", "10m", "now"],
        "the peak basis: the upload's 5 MB/s"
    );
    let learning = busy_network(|v| {
        v.basis = CapBasis::Learning;
        v.capacity = None;
        v.second.as_mut().expect("both directions").capacity = None;
        v.heat_now = None;
    });
    let laid = lay_snap(
        DeckScene::Busy,
        &learning,
        1119.0,
        2.0,
        Height::Three(51.0),
        &m,
    );
    assert_eq!(
        net_words(&laid),
        ["Network", "10m", "now"],
        "learning: no rate and no figure, the run's ends still named"
    );
    let gauge = laid.plan.block(BlockKind::Network).and_then(|b| b.gauge);
    let gx = (gauge.expect("a gauge").x * 2.0 + 0.5).floor() as f32;
    let dashed = laid
        .prims
        .iter()
        .filter(|p| matches!(p, DrawPrim::Dashed { x, .. } if *x == gx))
        .count();
    assert_eq!(dashed, 1, "the gauge is an absence, dashed");
}

/// The fill panels of Network's gauge: those in its fill's ink across its x.
fn gauge_fills(laid: &MergedLaid, ink: Rgb) -> usize {
    let (s, b) = (
        laid.plan.scale,
        laid.plan
            .block(BlockKind::Network)
            .expect("Full lays Network"),
    );
    let g = b.gauge.expect("Network's gauge");
    let (x0, x1) = (
        (g.x * s + 0.5).floor() as f32,
        (g.end() * s + 0.5).floor() as f32,
    );
    laid.prims
        .iter()
        .filter(|p| match **p {
            DrawPrim::Panel { x, w, fill, .. } => x >= x0 && x + w <= x1 && fill == rgba(ink, 0xFF),
            _ => false,
        })
        .count()
}

/// Network's gauge is jagged at its top only while the last 20 s swing past the spread
/// threshold (the gauge's 1.5 pt, as a share of the Mac lane's height), and flat just short of it.
#[test]
fn network_gauge_is_jagged_only_past_the_spread_threshold() {
    let m = FaceMetrics::helvetica_neue();
    let ink = dark().merged.heat_at(0.12);
    for (w, s) in NETWORK_SIZES {
        let at = format!("{w} pt {s}×");
        let busy = lay(DeckScene::Busy, w, s, Height::Three(51.0), &m);
        assert_eq!(busy.plan.laid, MergedForm::Full, "{at}");
        assert!(
            gauge_fills(&busy, ink) >= 3,
            "{at}: busy swings from 12% to 26%: stepped"
        );
        let lane = busy.plan.lanes.scopes.expect("three rows")[0].h;
        let threshold = 1.5 / lane;
        for (swing, jagged) in [(threshold * 0.8, false), (threshold * 1.25, true)] {
            let snap = busy_network(|v| {
                let mut ring = v.ring.to_vec();
                for c in ring.iter_mut().rev().take(2) {
                    c.max = c.mean.map(|mean| mean + swing * 125e6);
                }
                v.ring = ring.into();
            });
            let laid = lay_snap(DeckScene::Busy, &snap, w, s, Height::Three(51.0), &m);
            let fills = gauge_fills(&laid, ink);
            if jagged {
                assert!(fills >= 3, "{at}: a {swing:.3} swing is stepped: {fills}");
            } else {
                assert_eq!(fills, 1, "{at}: a {swing:.3} swing is flat");
            }
        }
    }
    let calm = lay(DeckScene::Calm, 1119.0, 2.0, Height::Three(51.0), &m);
    let calm_ink = dark().merged.heat_at(0.01);
    assert_eq!(gauge_fills(&calm, calm_ink), 1, "calm's 1% to 3% is flat");
}

/// Network's run is its last 10 minutes, 30 s a bar, oldest at its left: no bar is shorter than
/// one whose column read less (a bar's height only grows with its share), the tallest is where
/// the busy run peaked, and a run not yet read is all hollow.
#[test]
fn network_runs_the_last_ten_minutes_oldest_first() {
    let m = FaceMetrics::helvetica_neue();
    let snap = fixtures::snapshot(DeckScene::Busy);
    let want: Vec<f32> = snap.resources[Resource::Network.index()]
        .heat_columns(30, 20, snap.at_unix_ms)
        .iter()
        .map(|c| c.heat.expect("busy read every column"))
        .collect();
    let well = rgba(dark().merged.well, 0xFF);
    for (w, s) in NETWORK_SIZES {
        let at = format!("{w} pt {s}×");
        let px = |v: f64| (v * s + 0.5).floor() as f32;
        let run_in = |laid: &MergedLaid| {
            let run = laid.plan.block(BlockKind::Network).and_then(|b| b.run);
            let run = run.expect("Full lays Network's run");
            (px(run.at.x), px(run.at.end()))
        };
        let laid = lay(DeckScene::Busy, w, s, Height::Three(51.0), &m);
        let (x0, x1) = run_in(&laid);
        let foot = px(laid.plan.lanes.scopes.expect("three rows")[0].end());
        let mut bars: Vec<(f32, f32)> = laid
            .prims
            .iter()
            .filter_map(|p| match *p {
                DrawPrim::Panel {
                    x, y, w, h, fill, ..
                } if x >= x0 && x + w <= x1 && fill != well && y + h == foot => Some((x, h)),
                _ => None,
            })
            .collect();
        bars.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert_eq!(bars.len(), want.len(), "{at}: a bar each 30 s: {bars:?}");
        for (i, j) in (0..want.len()).flat_map(|i| (0..want.len()).map(move |j| (i, j))) {
            assert!(
                want[i] <= want[j] || bars[i].1 >= bars[j].1,
                "{at}: bar {i} ({}) is shorter than bar {j} ({}): {bars:?}",
                want[i],
                want[j]
            );
        }
        let tallest = |v: &[f32]| (0..v.len()).max_by(|&a, &b| v[a].total_cmp(&v[b]));
        let heights: Vec<f32> = bars.iter().map(|b| b.1).collect();
        assert_eq!(
            heights[tallest(&want).expect("bars")],
            heights.iter().copied().fold(0.0, f32::max),
            "{at}: the busiest 30 s is the tallest bar: {heights:?}"
        );
        assert!(
            heights.iter().any(|&h| h > heights[0]),
            "CONTROL: {at}: the bars differ: {heights:?}"
        );

        let absent = lay(DeckScene::Absent, w, s, Height::Three(51.0), &m);
        let (x0, x1) = run_in(&absent);
        let hollow = absent
            .prims
            .iter()
            .filter(|p| matches!(**p, DrawPrim::Dashed { x, w, .. } if x >= x0 && x + w <= x1))
            .count();
        assert_eq!(hollow, want.len(), "{at}: absent, every bar hollow");
    }
}

/// Network's block keeps to its prim budget in every scene: a gauge of at most 10 prims, three
/// words, and at most two a bar (its well and its fill, or one dashed outline).
#[test]
fn network_keeps_to_its_prim_budget() {
    let m = FaceMetrics::helvetica_neue();
    for scene in DeckScene::ALL {
        for (w, s) in NETWORK_SIZES {
            let laid = lay(scene, w, s, Height::Three(51.0), &m);
            let b = laid
                .plan
                .block(BlockKind::Network)
                .expect("Full lays Network");
            let mac = laid.plan.lanes.scopes.expect("three rows")[0];
            let px = |v: f64| (v * s + 0.5).floor() as f32;
            let (x0, y0) = (px(b.at.x), px(mac.y));
            let n = laid
                .prims
                .iter()
                .filter(|p| match **p {
                    DrawPrim::Panel { x, y, .. }
                    | DrawPrim::Stroke { x, y, .. }
                    | DrawPrim::Dashed { x, y, .. }
                    | DrawPrim::Text { x, baseline: y, .. } => x >= x0 - 1.0 && y >= y0,
                    _ => false,
                })
                .count();
            let bars = b.run.expect("Network's run").bars;
            assert!(
                n >= 3,
                "CONTROL: {scene:?} at {w} pt {s}× draws Network: {n}"
            );
            assert!(
                n <= 10 + 3 + 2 * bars,
                "{scene:?} at {w} pt {s}×: Network costs {n} prims"
            );
        }
    }
}

/// The card's ground, its tints and its rules: the ground and seam first, each scope lane's own
/// tint across the bands, and a rule per planned rule in the grid's ink, fainter where the bands
/// run on under it or past the divider. A rule between two scoped blocks stops above the bands so
/// they run on unbroken (§3); every other rule reaches the this-tab lane, and one under row A's
/// words starts below them. Row A alone draws only the rules it crosses, inside it, and no tint.
#[test]
fn the_skeleton_lays_ground_tints_and_rules() {
    let m = FaceMetrics::helvetica_neue();
    let ink = dark();
    for (w, s) in SIZES {
        let laid = lay(DeckScene::Calm, w, s, Height::Three(51.0), &m);
        let panels: Vec<(f32, f32, f32, f32, [u8; 4])> = laid
            .prims
            .iter()
            .filter_map(|p| match *p {
                DrawPrim::Panel {
                    x, y, w, h, fill, ..
                } => Some((x, y, w, h, fill)),
                _ => None,
            })
            .collect();
        let (cw, ch) = ((w * s) as f32, (51.0 * s).round() as f32);
        let seam = (laid.plan.lanes.seam.h * s).round() as f32;
        assert_eq!(
            panels[..2],
            [
                (0.0, 0.0, cw, ch - seam, rgba(ink.ground, 0xFF)),
                (0.0, ch - seam, cw, seam, rgba(ink.seam, 0xFF)),
            ],
            "{w} pt {s}×: the ground, then the seam"
        );
        let tints: Vec<_> = panels
            .iter()
            .filter(|p| {
                p.4 == rgba(ink.merged.all_tabs_tint, 0xFF)
                    || p.4 == rgba(ink.merged.this_tab_tint, 0xFF)
            })
            .collect();
        let bands = laid.plan.bands.expect("a three-row deck has bands");
        let scopes = laid.plan.lanes.scopes.expect("and scope lanes");
        let px = |v: f64| (v * s + 0.5).floor() as f32;
        assert_eq!(tints.len(), 2, "{w} pt {s}×");
        let own = [ink.merged.all_tabs_tint, ink.merged.this_tab_tint];
        for ((t, lane), tint) in tints.iter().zip(&scopes[1..]).zip(own) {
            assert_eq!(
                (t.0, t.0 + t.2, t.1 + t.3, t.4),
                (
                    px(bands.x),
                    px(bands.end()),
                    px(lane.end()),
                    rgba(tint, 0xFF)
                ),
                "{w} pt {s}×: a tint spans the bands in its own lane, in its own ink"
            );
            assert!(t.1 >= px(lane.y), "{w} pt {s}×: inside its lane");
        }
        let rules: Vec<_> = panels
            .iter()
            .filter(|p| [p.4[0], p.4[1], p.4[2]] == ink.grid)
            .collect();
        assert_eq!(
            rules.len(),
            laid.plan.rules.len(),
            "{w} pt {s}×: a rule each"
        );
        let faint = |r: &Rule| {
            r.kind == RuleKind::Block && (r.bottom == Lane::Mac || r.at.x >= bands.end())
        };
        for (drawn, r) in rules.iter().zip(&laid.plan.rules) {
            let alpha = if faint(r) { 204 } else { 255 };
            assert_eq!(drawn.4[3], alpha, "{w} pt {s}×: {r:?}");
            assert_eq!(drawn.0, px(r.at.x), "{r:?}");
            let (top, foot) = (drawn.1, drawn.1 + drawn.3);
            assert!(
                top > 0.0 && foot < px(scopes[2].end()),
                "{w} pt {s}×: {r:?} stays inside the deck's top and its last lane's foot"
            );
            if r.bottom == Lane::Mac {
                assert!(
                    foot <= px(scopes[1].y),
                    "{w} pt {s}×: the bands run on unbroken under {r:?}"
                );
            } else {
                assert!(
                    foot > px(scopes[2].y),
                    "{w} pt {s}×: {r:?} reaches the this-tab lane"
                );
            }
            if r.top == Lane::Mac {
                assert!(
                    top >= px(scopes[0].y),
                    "{w} pt {s}×: {r:?} leaves row A's words above it"
                );
            }
        }
        if laid.plan.laid == MergedForm::Full {
            let divider = laid
                .plan
                .rules
                .iter()
                .find(|r| r.kind == RuleKind::Divider)
                .expect("Full divides the bands from Disk");
            let before = laid.plan.block(BlockKind::Gpu).expect("GPU").at.end();
            assert!(divider.at.x > before, "the divider follows GPU");
        }

        let row_a = lay(DeckScene::Calm, w, s, Height::RowA, &m);
        let a = row_a.plan.lanes.a;
        let crossed: Vec<&Rule> = row_a
            .plan
            .rules
            .iter()
            .filter(|r| r.top == Lane::A)
            .collect();
        let drawn: Vec<(f32, f32, f32)> = row_a
            .prims
            .iter()
            .filter_map(|p| match *p {
                DrawPrim::Panel { x, y, h, fill, .. }
                    if [fill[0], fill[1], fill[2]] == ink.grid =>
                {
                    Some((x, y, y + h))
                }
                DrawPrim::Panel { fill, .. } => {
                    assert!(
                        fill != rgba(ink.merged.all_tabs_tint, 0xFF)
                            && fill != rgba(ink.merged.this_tab_tint, 0xFF),
                        "{w} pt {s}×: row A alone draws no tint"
                    );
                    None
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            drawn.iter().map(|d| d.0).collect::<Vec<_>>(),
            crossed.iter().map(|r| px(r.at.x)).collect::<Vec<_>>(),
            "{w} pt {s}×: row A alone draws the rules it crosses"
        );
        for (x, top, foot) in drawn {
            assert!(
                top > px(a.y) && foot < px(a.end()),
                "{w} pt {s}×: the rule at {x} stays inside row A"
            );
        }
    }
}

/// The prims the CPU block alone draws for `snap` on `platform` in a 51 pt card `w` × `s`, its
/// plan, and its block.
fn cpu_block(
    snap: &Snapshot,
    platform: Platform,
    w: f64,
    s: f64,
    m: &FaceMetrics,
) -> (Vec<DrawPrim>, MergedPlan, merged_plan::Block) {
    let h = Height::Three(51.0);
    let mut input = input_of(DeckScene::Busy, snap, dark());
    input.locale.platform = platform;
    let planner = MergedPlanner::new(h.px(s).1, s, MachineShape::of(Some(snap), platform), m);
    let g = geom(w, s, h);
    let mut card = Card::new(&g, &input, m, &planner, true);
    let glance = CpuGlance::of(snap, platform);
    card.cpu(Some(snap), glance.as_ref());
    let block = card
        .plan
        .block(BlockKind::Cpu)
        .expect("a CPU block")
        .clone();
    (card.p.prims, card.plan, block)
}

/// The panels of `prims` in `fill`, as `(x, y, w, h)`, whose left edge lies in `span`'s device
/// px at `s`.
fn panels_at(prims: &[DrawPrim], fill: Rgb, span: Span, s: f64) -> Vec<Rect> {
    let (x0, x1) = ((span.x * s).floor() as f32, (span.end() * s).ceil() as f32);
    prims
        .iter()
        .filter_map(|p| match *p {
            DrawPrim::Panel {
                x,
                y,
                w,
                h,
                fill: f,
                ..
            } if f == rgba(fill, 0xFF) && (x0..x1).contains(&x) => Some((x, y, w, h)),
            _ => None,
        })
        .collect()
}

/// The CPU's parts: one tile per core, kind by kind as the OS lists them, the fastest kind's
/// wider and taller, each kind after its letter, every tile inside the parts on the Mac lane's
/// foot, and no two touching; one kind draws no letter (§5).
#[test]
fn the_cpu_lays_a_tile_a_core_kind_by_kind() {
    let m = FaceMetrics::helvetica_neue();
    let well = dark().merged.well;
    let busy = fixtures::snapshot(DeckScene::Busy);
    let mut one_kind = busy.clone();
    if let Some(cpu) = one_kind.cpu.as_mut() {
        cpu.cores.truncate(10);
        cpu.clusters = vec![aterm_vitals::model::ClusterView {
            name: "Performance".into(),
            short: "P".into(),
            cores: (0..10).collect(),
        }];
    }
    let mut checked = 0;
    for (w, s) in SIZES {
        for (snap, kinds) in [(&busy, &[6, 12][..]), (&one_kind, &[10][..])] {
            let (prims, plan, b) = cpu_block(snap, Platform::MacOs, w, s, &m);
            let at = format!("{kinds:?} at {w} pt {s}×, {:?}", plan.laid);
            let parts = b.parts.expect("the CPU's parts");
            let Some(Grid::Tiles { groups, .. }) = &b.grid else {
                assert!(
                    s < 2.0 || plan.laid != MergedForm::Full,
                    "{at}: the full deck at 2× tiles every core"
                );
                continue;
            };
            let tiles = panels_at(&prims, well, parts, s);
            assert_eq!(
                tiles.len(),
                kinds.iter().sum::<usize>(),
                "{at}: a tile a core"
            );
            let foot = (plan.lanes.scopes.expect("three rows")[0].end() * s).round() as f32;
            let (left, right) = (
                (parts.x * s).floor() as f32,
                (parts.end() * s).ceil() as f32,
            );
            for t in &tiles {
                assert!(
                    t.0 >= left && t.0 + t.2 <= right + 1.0,
                    "{at}: {t:?} in the parts"
                );
                assert_eq!(t.1 + t.3, foot, "{at}: {t:?} stands on the lane's foot");
            }
            for pair in tiles.windows(2) {
                assert!(
                    pair[1].0 >= pair[0].0 + pair[0].2 + 1.0,
                    "{at}: {pair:?} touch"
                );
            }
            checked += 1;
            let letters: Vec<(String, f32)> = prims
                .iter()
                .filter_map(|p| match p {
                    DrawPrim::Text { s, x, .. } => Some((s.clone(), *x)),
                    _ => None,
                })
                .filter(|(s, _)| s == "S" || s == "P")
                .collect();
            if kinds.len() == 1 {
                assert!(letters.is_empty(), "{at}: one kind needs no letter");
                continue;
            }
            assert_eq!(groups.len(), 2, "{at}");
            let (fast, slow) = tiles.split_at(kinds[0]);
            assert!(
                fast.iter()
                    .all(|f| slow.iter().all(|p| f.2 > p.2 && f.3 > p.3)),
                "{at}: Super's tiles are wider and taller than Performance's"
            );
            assert_eq!(
                letters.iter().map(|l| l.0.as_str()).collect::<Vec<_>>(),
                ["S", "P"],
                "{at}"
            );
            assert!(
                letters[0].1 < fast[0].0
                    && fast[kinds[0] - 1].0 < letters[1].1
                    && letters[1].1 < slow[0].0,
                "{at}: each kind after its letter: {letters:?}"
            );
        }
    }
    assert!(checked >= 3, "CONTROL: {checked} lays checked");
}

/// Work waiting past capacity (§5): the dashed wall always, an orange slat per core's worth past
/// it up to ten, the gauge's orange top and `~8 waiting` in orange over them; none of the orange
/// when nothing waits, and none on Linux, whose load average counts disk waits (§4: orange always
/// carries its words).
#[test]
fn the_wall_holds_back_what_waits() {
    let m = FaceMetrics::helvetica_neue();
    let ink = dark();
    let busy = fixtures::snapshot(DeckScene::Busy);
    let mut swamped = busy.clone();
    swamped.facts.load1 = Some(40.2);
    let calm = fixtures::snapshot(DeckScene::Calm);
    for (w, s) in SIZES {
        for (name, snap, platform, slats, said) in [
            ("busy", &busy, Platform::MacOs, 8, Some("~8 waiting")),
            (
                "swamped",
                &swamped,
                Platform::MacOs,
                10,
                Some("~22 waiting"),
            ),
            ("calm", &calm, Platform::MacOs, 0, None),
            ("busy on Linux", &busy, Platform::Linux, 0, None),
        ] {
            let (prims, plan, b) = cpu_block(snap, platform, w, s, &m);
            let at = format!("{name} at {w} pt {s}×, {:?}", plan.laid);
            let over = b.over.expect("the slats' column");
            let orange = |span: Span| panels_at(&prims, ink.over, span, s).len();
            assert_eq!(orange(over), slats, "{at}: the slats");
            if slats == 0 {
                assert_eq!(orange(b.at), 0, "{at}: no orange anywhere in the block");
            }
            let wall = (b.wall.expect("the wall") * s).round() as f32;
            let walls = prims
                .iter()
                .filter(|p| matches!(p, DrawPrim::Dashed { x, w, .. } if *w == 0.0 && (*x - wall).abs() <= 1.0))
                .count();
            assert_eq!(walls, 1, "{at}: the wall");
            if let Some(gauge) = b.gauge {
                let gx = (gauge.x * s + 0.5).floor() as f32;
                let topped = prims
                    .iter()
                    .any(|p| matches!(p, DrawPrim::ClipPush { x, .. } if *x == gx));
                assert_eq!(topped, said.is_some(), "{at}: the gauge's orange top");
            }
            let mut input = input_of(DeckScene::Busy, snap, ink);
            input.locale.platform = platform;
            let planner = MergedPlanner::new(
                Height::Three(51.0).px(s).1,
                s,
                MachineShape::of(Some(snap), platform),
                &m,
            );
            let geom = geom(w, s, Height::Three(51.0));
            let laid = merged_lay(&planner, &geom, &input, &m, true);
            let words: Vec<(String, Rgb, TextFace)> = texts(&laid)
                .into_iter()
                .filter(|t| t.0.ends_with(" waiting"))
                .collect();
            let want: Vec<(String, Rgb, TextFace)> = said
                .map(|w| (w.to_owned(), ink.merged.over_text, TextFace::Ui))
                .into_iter()
                .collect();
            assert_eq!(words, want, "{at}: the orange words");
        }
    }
}

/// A CPU of 8 to 32 cores in one, two or three kinds keeps the slot the planner gives the
/// reference Mac: each core a tile of at least two device px with a gap, inside the parts, or,
/// where tiles would be thinner, one tank across the parts at the whole CPU's level.
#[test]
fn eight_to_thirty_two_cores_fit_the_slot_or_fill_one_tank() {
    let m = FaceMetrics::helvetica_neue();
    let well = dark().merged.well;
    let busy = fixtures::snapshot(DeckScene::Busy);
    let reference: Vec<(Span, Option<Span>)> = SIZES
        .iter()
        .map(|&(w, s)| {
            let shape = MachineShape::reference(Platform::MacOs);
            let planner = MergedPlanner::new(Height::Three(51.0).px(s).1, s, shape, &m);
            let plan = planner.plan(w, false);
            let b = plan.block(BlockKind::Cpu).expect("the reference Mac's CPU");
            (b.at, b.parts)
        })
        .collect();
    let (mut tiled, mut tanked) = (0, 0);
    for n in 8..=32_usize {
        for split in [
            vec![n],
            vec![n / 3, n - n / 3],
            vec![n / 4, n / 4, n - 2 * (n / 4)],
        ] {
            let mut snap = busy.clone();
            let cpu = snap.cpu.as_mut().expect("the CPU");
            cpu.cores = (0..n).map(|i| cpu.cores[i % cpu.cores.len()]).collect();
            let mut next = 0;
            cpu.clusters = split
                .iter()
                .zip(["S", "P", "E"])
                .map(|(&k, short)| {
                    next += k;
                    aterm_vitals::model::ClusterView {
                        name: short.into(),
                        short: short.into(),
                        cores: (next - k..next).collect(),
                    }
                })
                .collect();
            for (&(w, s), slot) in SIZES.iter().zip(&reference) {
                let (prims, plan, b) = cpu_block(&snap, Platform::MacOs, w, s, &m);
                let at = format!("{split:?} at {w} pt {s}×, {:?}", plan.laid);
                assert_eq!((b.at, b.parts), *slot, "{at}: the reference Mac's slot");
                let parts = b.parts.expect("the CPU's parts");
                let wells = panels_at(&prims, well, parts, s);
                let (left, right) = (
                    (parts.x * s).floor() as f32,
                    (parts.end() * s).ceil() as f32,
                );
                for t in &wells {
                    assert!(
                        t.0 >= left && t.0 + t.2 <= right + 1.0,
                        "{at}: {t:?} in the parts"
                    );
                }
                match b.grid {
                    Some(Grid::Tiles { .. }) => {
                        tiled += 1;
                        assert_eq!(wells.len(), n, "{at}: a tile a core");
                        assert!(wells.iter().all(|t| t.2 >= 2.0), "{at}: {wells:?}");
                        for pair in wells.windows(2) {
                            assert!(pair[1].0 > pair[0].0 + pair[0].2, "{at}: {pair:?} touch");
                        }
                    }
                    _ => {
                        tanked += 1;
                        assert_eq!(wells.len(), 1, "{at}: one tank");
                        let t = wells[0];
                        assert!(
                            t.0 <= left + 1.0 && t.0 + t.2 >= right - 1.0,
                            "{at}: the tank spans the parts: {t:?}"
                        );
                    }
                }
            }
        }
    }
    assert!(
        tiled > 0 && tanked > 0,
        "CONTROL: {tiled} tiled, {tanked} tanked"
    );
}

/// Every run a Full plan lays over its end words keeps room for them once the painter snaps it
/// to device px, so the mark that names them never refuses it and the run never vanishes (§4: a
/// run whose ends are not named is not drawn): from Full's threshold to 200 pt past it, in both
/// faces, at 1× and 2×, at the least, the mockups' and the greatest deck height.
#[test]
fn every_planned_run_names_its_ends_once_snapped() {
    let faces = [
        ("Helvetica Neue", FaceMetrics::helvetica_neue()),
        ("the test stack", FaceMetrics::test_stack()),
    ];
    let snap = fixtures::snapshot(DeckScene::Busy);
    let input = input_of(DeckScene::Busy, &snap, dark());
    let (mut runs, mut failed) = (0, Vec::new());
    for (face, m) in &faces {
        for s in [1.0, 2.0] {
            for pt in DECKS_PT {
                let h = Height::Three(pt);
                let planner = planner(&snap, s, h, m);
                let full = planner.thresholds()[0];
                for w in (0..400).map(|i| full + f64::from(i) / 2.0) {
                    let g = geom(w, s, h);
                    let card = Card::new(&g, &input, m, &planner, false);
                    let size = card.words_px();
                    for b in &card.plan.blocks {
                        let Some(run) = b.run.as_ref().filter(|r| !r.ends_beside) else {
                            continue;
                        };
                        runs += 1;
                        let (x, lead, base, ends) = card.run_ends_at(run);
                        let mut p = Painter::new(&input.inks, g.scale, g.font_px, input.dwc);
                        let named =
                            Marks::new(&mut p, false).run_ends(x, lead, base, ends, size, m);
                        if !named && failed.len() < 20 {
                            failed.push(format!(
                                "{face}, {s}×, {pt} pt deck, {w} pt: {:?}'s run {run:?}",
                                b.kind
                            ));
                        }
                    }
                }
            }
        }
    }
    assert!(runs >= 12 * 400, "CONTROL: {runs} runs checked");
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// Before the CPU is read its parts and gauge are dashed outlines and its run hollow, and it
/// writes nothing: no letter, no figure, no orange.
#[test]
fn an_unread_cpu_is_dashed_and_says_nothing() {
    let m = FaceMetrics::helvetica_neue();
    let absent = fixtures::snapshot(DeckScene::Absent);
    let (prims, plan, b) = cpu_block(&absent, Platform::MacOs, 1119.0, 2.0, &m);
    assert_eq!(plan.laid, MergedForm::Full);
    let dashed = prims
        .iter()
        .filter(|p| matches!(p, DrawPrim::Dashed { .. }))
        .count();
    assert_eq!(
        dashed,
        2 + RUN_BARS,
        "the parts, the gauge and each bucket of the run"
    );
    let words: Vec<&str> = prims
        .iter()
        .filter_map(|p| match p {
            DrawPrim::Text { s, .. } => Some(s.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(words, ["10m", "now"], "only the run's ends");
    assert!(b.parts.is_some() && b.gauge.is_some(), "CONTROL: {b:?}");
}

/// The CPU block's prims stay within its budget in every form: 4 a tile, 3 a run bar, and 35
/// for the rest (2 letters, the wall, 10 slats, the gauge's 15 at most, the figure and the
/// run's 6 for its ends and bracket).
#[test]
fn the_cpu_keeps_to_its_prim_budget() {
    let m = FaceMetrics::helvetica_neue();
    let busy = fixtures::snapshot(DeckScene::Busy);
    let mut swamped = busy.clone();
    swamped.facts.load1 = Some(40.2);
    for scene in DeckScene::ALL {
        let snap = if scene == DeckScene::Busy {
            swamped.clone()
        } else {
            fixtures::snapshot(scene)
        };
        for (w, s) in SIZES {
            let (prims, plan, _) = cpu_block(&snap, Platform::MacOs, w, s, &m);
            let cores = snap.cpu.as_ref().map_or(0, |c| c.total());
            let budget = 4 * cores + 3 * RUN_BARS + 35;
            eprintln!(
                "{scene:?} at {w} pt {s}× ({:?}): {} prims",
                plan.laid,
                prims.len()
            );
            assert!(
                prims.len() <= budget,
                "{scene:?} at {w} pt {s}×: {} prims, budget {budget}",
                prims.len()
            );
        }
    }
}

fn hit_at(hits: &[Hit], x: f64, y: f64, s: f64) -> Option<HitKind> {
    let (x, y) = ((x * s) as f32, (y * s) as f32);
    hits.iter()
        .find(|h| crate::stats_paint::contains(h.rect, f64::from(x), f64::from(y)))
        .map(|h| h.kind)
}

/// Each block names its instrument in each lane: a scoped block's row A and Mac lane the Mac,
/// its all-tabs lane aterm and its this-tab lane the tab; Disk and Network the Mac over their
/// height. The rail and the rules name nothing, the form's blocks are named in order, and the
/// regions are the same whatever the snapshot reads.
#[test]
fn merged_hit_map() {
    let m = FaceMetrics::helvetica_neue();
    for (w, s) in SIZES {
        let mut first: Option<Vec<Hit>> = None;
        for scene in DeckScene::ALL {
            let at = format!("{scene:?} at {w} pt {s}×");
            let laid = lay(scene, w, s, Height::Three(51.0), &m);
            let (plan, hits) = (&laid.plan, &laid.hits);
            match &first {
                Some(seen) => assert_eq!(seen, hits, "{at}: the readings move no region"),
                None => first = Some(hits.clone()),
            }
            let sc = plan.lanes.scopes.expect("three rows");
            let mid = |b: Band| b.y + b.h / 2.0;
            let lanes = [plan.lanes.a, sc[0], sc[1], sc[2]];
            for b in &plan.blocks {
                let row = |bar| Some(HitKind::Row(Instrument::of(resource(b.kind)), bar));
                let bars = if scoped(b.kind) {
                    [Bar::Global, Bar::Global, Bar::Aterm, Bar::Tab]
                } else {
                    [Bar::Global; 4]
                };
                let inside = 1.0 / s;
                for x in [b.at.x + inside, b.at.x + b.at.w / 2.0, b.at.end() - inside] {
                    for (lane, bar) in lanes.iter().zip(bars) {
                        assert_eq!(
                            hit_at(hits, x, mid(*lane), s),
                            row(bar),
                            "{at}: {:?} at {x} in {lane:?}",
                            b.kind
                        );
                    }
                }
            }
            let rail = plan.rail.x + plan.rail.w / 2.0;
            for lane in lanes {
                assert_eq!(hit_at(hits, rail, mid(lane), s), None, "{at}: the rail");
            }
            for r in &plan.rules {
                let x = r.at.x + r.at.w / 2.0;
                assert_eq!(hit_at(hits, x, mid(sc[0]), s), None, "{at}: {r:?}");
            }
            let mut named: Vec<Instrument> = Vec::new();
            for h in hits {
                if !named.contains(&h.instrument()) {
                    named.push(h.instrument());
                }
            }
            let blocks: Vec<Instrument> = plan
                .blocks
                .iter()
                .map(|b| Instrument::of(resource(b.kind)))
                .collect();
            assert_eq!(named, blocks, "{at}");
            assert!(!blocks.is_empty(), "CONTROL: {at} lays blocks");
        }
        let laid = lay(DeckScene::Busy, w, s, Height::RowA, &m);
        let a = laid.plan.lanes.a;
        for b in &laid.plan.blocks {
            let row = Some(HitKind::Row(Instrument::of(resource(b.kind)), Bar::Global));
            let x = b.at.x + b.at.w / 2.0;
            assert_eq!(
                hit_at(&laid.hits, x, a.y + a.h / 2.0, s),
                row,
                "row A at {w} pt"
            );
        }
        assert_eq!(
            laid.hits.len(),
            laid.plan.blocks.len(),
            "row A: one region a block"
        );
        let three = lay(DeckScene::Busy, w, s, Height::Three(51.0), &m);
        assert_eq!(
            three.hits.len(),
            Lane::ALL.len() * three.plan.blocks.len(),
            "three rows: a region a block and lane"
        );
    }
}

/// A snapshot lays the same prims, the same regions and the same pixels every time, whether its
/// planner is built for the call or kept from the last.
#[test]
fn the_same_snapshot_lays_the_same_pixels() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let light = crate::stats_deck::paint_tests::light_inks();
    for scene in DeckScene::ALL {
        for (w, s) in SIZES {
            for h in [Height::Three(51.0), Height::RowA] {
                for inks in [dark(), light] {
                    let g = geom(w, s, h);
                    let paint = || {
                        let snap = fixtures::snapshot(scene);
                        let input = input_of(scene, &snap, inks);
                        let (prims, hits) = merged_paint(&g, &input);
                        let raster = crate::tray_raster::rasterize_tray(
                            &prims, g.w as u32, g.h as u32, 1.0, [0; 4],
                        );
                        (prims_fp(&prims), prims.len(), hits, raster)
                    };
                    let (a, b) = (paint(), paint());
                    assert!(a.1 > 2, "CONTROL: {scene:?} draws past its ground");
                    assert!(
                        a == b,
                        "{scene:?} at {w} pt {s}× {h:?}: two paints of one snapshot differ"
                    );
                }
            }
        }
    }
}

/// The merged deck's accessibility group is its own: titled "Stats deck", the (empty) needs
/// first, then a meter for each block in the deck's order, then the cores, where v2's has no
/// needs and its own rows.
#[cfg(a11y_tree)]
#[test]
fn the_reader_meets_the_needs_then_each_block() {
    use std::sync::Arc;

    use crate::stats_deck::{DeckStyle, StatsBars, deck_px};

    let _attached = crate::stats_deck::tests::attach_stats();
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let snap = fixtures::snapshot(DeckScene::Busy);
    app.vitals.snapshot = Some(Arc::new(snap.clone()));
    app.windows.get_mut(&wid).expect("window").metrics.pad = 24;
    for style in [DeckStyle::V2, DeckStyle::Merged] {
        let sys = app.config.sys.get_or_insert_with(Default::default);
        sys.stats_bars = Some("deck".into());
        sys.stats_deck_style = Some(style.word().into());
        assert_eq!(app.deck_style(), style);
        let (_, cell) = app.win_cell_size(wid);
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.metrics.head = deck_px(StatsBars::Deck, style, ws.deck_form, ws.scale, cell);
        let stats = app.stats_a11y(wid).expect("the deck is read");
        let group = stats.group;
        let names: Vec<&str> = group.meters.iter().map(|m| m.name.as_str()).collect();
        match style {
            DeckStyle::V2 => {
                assert_eq!(group.name, "Stats deck, last 10 minutes");
                assert_eq!(group.needs, None);
                assert_eq!(names[0], "CPU", "v2's rows");
            }
            DeckStyle::Merged => {
                assert_eq!(group.name, "Stats deck");
                let needs = group.needs.expect("the needs group");
                assert_eq!(
                    (needs.name.as_str(), needs.needs.len()),
                    (words::NEEDS_RAIL_CALM, 0)
                );
                assert_eq!(
                    names,
                    ["Processes", "CPU", "Memory", "GPU", "Disk", "Network"]
                );
                for (meter, kind) in group.meters.iter().zip(MergedForm::Full.blocks()) {
                    let res = resource(*kind);
                    let heat = snap.resources[res.index()].heat_now;
                    assert_eq!(meter.value, crate::stats_deck::meter_pct(heat), "{kind:?}");
                    assert!(!meter.words.is_empty(), "{kind:?} is said");
                }
            }
        }
        assert_eq!(
            (group.cores_name, group.cores.len()),
            ("18 CPU cores".to_owned(), 18),
            "{style:?}: every core is read"
        );
    }
}
