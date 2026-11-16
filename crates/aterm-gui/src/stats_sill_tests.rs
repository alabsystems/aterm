// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::stats_deck::deck_paint;
use crate::stats_deck::paint_tests::owner_scenes::{
    OWNER_W, Owner, owner_geom_at, owner_input, owner_scene,
};
use crate::stats_deck::paint_tests::{FRONT, light_inks};
use crate::stats_deck::tests::{dark_inks, input, snapshot};
use crate::stats_plan::DeckForm;
use aterm_vitals::ink::VitalsInks;
use aterm_vitals::model::ScopeView;

/// A 2× window: a 24 px bottom pad, 2,238 px wide, whose card keeps the pad's
/// top 18 px over the 6 px drop border.
const GEOM: DeckGeom = DeckGeom {
    w: 2238.0,
    h: 18.0,
    pad: 24.0,
    scale: 2.0,
    font_px: 26.0,
};

/// A large window at 1× (the Linux and Windows phases): a 12 px pad over a
/// 6 px drop border leaves a 6 px card.
const GEOM_1X: DeckGeom = DeckGeom {
    w: 1600.0,
    h: 6.0,
    pad: 12.0,
    scale: 1.0,
    font_px: 13.0,
};

#[test]
fn the_sill_is_the_bottom_pad_and_needs_eight_points_of_it() {
    assert_eq!(sill_px(24, 2.0), 24);
    assert_eq!(sill_px(12, 1.0), 12);
    assert_eq!(sill_px(15, 2.0), 0, "7.5 pt of pad hides the sill");
    assert_eq!(sill_px(16, 2.0), 16);
}

#[test]
fn a_translucent_floor_puts_a_plate_behind_each_instrument_only() {
    let snap = snapshot();
    let mut i = input(&snap);
    let plate = rgba(i.inks.ground, 235);
    let is_plate = |p: &DrawPrim| matches!(p, DrawPrim::Panel { fill, .. } if *fill == plate);
    let (bare, hits) = sill_paint(&GEOM, &i);
    assert!(!hits.is_empty());
    assert_eq!(bare.iter().filter(|p| is_plate(p)).count(), 0);
    i.plate = Some(235);
    let (plated, plated_hits) = sill_paint(&GEOM, &i);
    assert_eq!(plated_hits, hits, "a plate moves no instrument");
    assert_eq!(plated.len(), bare.len() + hits.len());
    assert!(
        plated[..hits.len()].iter().all(is_plate),
        "the plates are under the instruments"
    );
    assert_eq!(format!("{:?}", &plated[hits.len()..]), format!("{bare:?}"));
}

#[test]
fn the_sill_stays_in_the_pad_and_leaves_the_drop_border() {
    let snap = snapshot();
    let mut i = input(&snap);
    i.tab_count = 2;
    let (prims, _) = sill_paint(&GEOM, &i);
    assert!(!prims.is_empty());
    let mut clip_depth = 0;
    for p in &prims {
        match p {
            DrawPrim::ClipPush { y, h, .. } => {
                clip_depth += 1;
                assert!(*y >= -1.0 && *y + *h <= 19.0, "{p:?}");
            }
            DrawPrim::ClipPop => clip_depth -= 1,
            DrawPrim::Panel { y, h, .. } | DrawPrim::Stroke { y, h, .. } if clip_depth == 0 => {
                assert!(*y >= -1.0 && *y + *h <= 19.0, "{p:?}");
            }
            DrawPrim::Text { .. } => panic!("the sill draws no words but the well's counts"),
            _ => {}
        }
    }
}

#[test]
fn a_stale_sill_retains_its_readings_and_hit_regions() {
    let snap = snapshot();
    let mut shown = input(&snap);
    let (fresh, hits) = sill_paint(&GEOM, &shown);
    shown.live = crate::stats_live::DeckLive::STALE;
    let (old, old_hits) = sill_paint(&GEOM, &shown);
    assert_eq!(hits, old_hits);
    assert_eq!(fresh.len(), old.len(), "held meters do not disappear");
    assert_ne!(
        format!("{fresh:?}"),
        format!("{old:?}"),
        "stale readings are distinguished"
    );
}

#[test]
fn the_lanes_sit_at_two_eight_and_fourteen_at_2x() {
    assert_eq!(lane_rows(2.0), [(2.0, 4.0), (8.0, 4.0), (14.0, 4.0)]);
    assert_eq!(lane_rows(1.0), [(1.0, 2.0), (4.0, 2.0), (7.0, 2.0)]);
}

#[test]
fn the_tab_lane_lights_at_two_tabs_and_moves_nothing_else() {
    let snap = snapshot();
    let one = input(&snap);
    let mut two = input(&snap);
    two.tab_count = 2;
    let (_, h1) = sill_paint(&GEOM, &one);
    let (_, h2) = sill_paint(&GEOM, &two);
    assert!(!h1.iter().any(|h| h.bar() == Bar::Tab));
    assert!(h2.iter().any(|h| h.bar() == Bar::Tab));
    for h in &h1 {
        assert!(h2.contains(h), "{h:?} moved");
    }
}

/// A split front tab's CPU lane is the whole tab's: as long as all tabs' (aterm itself idles,
/// so both are claude's 1 core and cargo's 2), where the focused pane alone falls short.
#[test]
fn a_split_tabs_lane_is_the_whole_tab() {
    let snap = crate::stats_deck::tests::split_tab(7, 8);
    let fill = |panes: Vec<u64>, ink: fn(&DeckInput<'_>) -> aterm_vitals::ink::Rgb| {
        let i = DeckInput {
            tab_panes: panes,
            tab_count: 2,
            ..input(&snap)
        };
        let want = rgba(ink(&i), 0xFF);
        let (prims, _) = sill_paint(&GEOM, &i);
        prims
            .windows(2)
            .find_map(|w| match w {
                [DrawPrim::ClipPush { w, .. }, DrawPrim::Panel { fill, .. }] if *fill == want => {
                    Some(*w)
                }
                _ => None,
            })
            .expect("the lane's fill")
    };
    let tab = |i: &DeckInput<'_>| i.inks.tab;
    let all = fill(vec![8, 7], |i| i.inks.aterm);
    assert!(all > 1.0, "{all}");
    assert_eq!(fill(vec![8, 7], tab), all);
    assert_eq!(fill(vec![7, 8], tab), all);
    let focused = fill(vec![8], tab);
    assert!(focused < all, "CONTROL: {focused} vs {all}");
}

#[test]
fn the_width_law_folds_disk_then_memory_then_cores() {
    assert_eq!(
        Fold::of(1095.0),
        Fold {
            disk: true,
            memory: true,
            per_core: true,
            minimal: false
        }
    );
    assert!(!Fold::of(700.0).disk && Fold::of(700.0).memory);
    assert!(!Fold::of(500.0).memory && Fold::of(500.0).per_core);
    assert!(!Fold::of(300.0).per_core && !Fold::of(300.0).minimal);
    assert!(Fold::of(200.0).minimal);
    let snap = snapshot();
    let i = input(&snap);
    let instruments = |w_pt: f32| {
        let g = DeckGeom {
            w: w_pt * 2.0 + 48.0,
            ..GEOM
        };
        let (_, hits) = sill_paint(&g, &i);
        hits.iter().map(|h| h.instrument()).collect::<Vec<_>>()
    };
    let wide = instruments(1095.0);
    assert!(wide.contains(&Instrument::Disk) && wide.contains(&Instrument::Memory));
    let mid = instruments(700.0);
    assert!(!mid.contains(&Instrument::Disk) && mid.contains(&Instrument::Memory));
    let narrow = instruments(500.0);
    assert!(!narrow.contains(&Instrument::Memory) && narrow.contains(&Instrument::Cpu));
}

#[test]
fn the_core_row_draws_one_square_a_core() {
    let snap = snapshot();
    let (prims, _) = sill_paint(&GEOM, &input(&snap));
    let squares = prims
        .iter()
        .filter(|p| matches!(p, DrawPrim::Panel { w, h, radius, .. } if *w == 16.0 && *h == 16.0 && *radius >= 2.0))
        .count();
    assert!(squares >= 10, "{squares}");
}

#[test]
fn a_pinned_machine_fills_the_end_bracket() {
    let mut snap = snapshot();
    if let Some(cpu) = snap.cpu.as_mut() {
        cpu.busy = TrendView::flat(10.0);
    }
    let i = input(&snap);
    let hot = rgba(i.inks.load[4], 0xFF);
    let (prims, _) = sill_paint(&GEOM, &i);
    assert!(
        prims
            .iter()
            .any(|p| matches!(p, DrawPrim::Panel { fill, h, .. } if *fill == hot && *h == 10.0))
    );
}

/// The sill's core tiles keep the load ramp of the original scheme (979d8c3559) at every
/// load, a hot one too, and draw no glow (the owner, 2026-10-26: "go back to the orignal
/// color scheme"); orange stays on the dial. CONTROL: no tile takes a warm stop.
#[test]
fn hot_cores_draw_the_sills_load_ramp_and_no_glow() {
    let mut snap = snapshot();
    if let Some(cpu) = snap.cpu.as_mut() {
        for core in &mut cpu.cores {
            core.load = TrendView::flat(0.95);
        }
    }
    let i = input(&snap);
    let hot = rgba(i.inks.load_at(0.95), 0xFF);
    let (prims, _) = sill_paint(&GEOM, &i);
    let fills: Vec<[u8; 4]> = prims
        .iter()
        .filter_map(|p| match *p {
            DrawPrim::Panel { fill, .. } => Some(fill),
            _ => None,
        })
        .collect();
    assert!(
        fills.contains(&hot),
        "a hot core's tile draws load_at(0.95)"
    );
    for warm in &i.inks.thermal[2..] {
        assert!(
            !fills.contains(&rgba(*warm, 0xFF)),
            "CONTROL: no tile draws the dial's warm stop {warm:?}"
        );
    }
    // No glow: a hot sill strokes what a cold one does, every stroke inside the sill's pad.
    let mut cold = snapshot();
    if let Some(cpu) = cold.cpu.as_mut() {
        for core in &mut cpu.cores {
            core.load = TrendView::flat(0.05);
        }
    }
    let (cold_prims, _) = sill_paint(&GEOM, &input(&cold));
    let strokes = |prims: &[DrawPrim]| -> Vec<(f32, f32, f32, f32)> {
        prims
            .iter()
            .filter_map(|p| match *p {
                DrawPrim::Stroke { x, y, w, h, .. } => Some((x, y, w, h)),
                _ => None,
            })
            .collect()
    };
    assert_eq!(
        strokes(&prims),
        strokes(&cold_prims),
        "a hot core strokes no glow"
    );
    for (_, y, _, h) in strokes(&prims) {
        assert!(
            y >= -1.0 && y + h <= 19.0,
            "a stroke at {y}+{h} past the sill's pad"
        );
    }
}

/// The card stops where the drop-target border begins, whatever the window:
/// the 2× pad keeps the three lanes' 18 px, a large window at 1× keeps 6 px
/// over its 6 px border, a small one 9 px over 3 px.
#[test]
fn the_sill_card_leaves_the_drop_border_to_the_border() {
    assert_eq!(sill_card_px(24, 2238, 1900), 18);
    assert_eq!(sill_card_px(12, 1600, 1300), 6);
    assert_eq!(sill_card_px(12, 900, 640), 9);
    assert_eq!(sill_card_px(4, 1600, 1300), 0, "no card under the border");
}

/// The App places the sill's card from the top of the bottom pad to the top
/// of the drop border, so its raster and its hit map end there.
#[cfg(unix)]
#[test]
fn the_app_ends_the_sill_card_at_the_drop_border() {
    let _attached = crate::stats_deck::tests::attach_stats();
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    app.config
        .sys
        .get_or_insert_with(Default::default)
        .stats_bars = Some("sill".into());
    // A 24 px bottom pad (a test window's own is thinner).
    app.windows.get_mut(&wid).expect("window").metrics.pad = 24;
    let f = app.deck_frame(wid).expect("the sill draws");
    let frame_h = app.stats_frame_h(wid);
    let pad = app.win_pad(wid);
    let border = aterm_render::drop_overlay_border_px(f.geom.w as usize, frame_h, 0);
    assert!(border >= 2);
    assert_eq!(f.dy as usize, frame_h - pad, "the card starts at the pad");
    assert_eq!(
        f.dy as usize + f.geom.h as usize,
        frame_h - border,
        "the card ends where the drop border begins"
    );
}

/// At 1× the lanes yield to the drop border bottom-up: on a 6 px card the Tab
/// lane yields at two tabs and the Global and aterm lanes stay where they are;
/// on a 9 px card (a small window's 3 px border) all three fit.
#[test]
fn at_1x_the_tab_lane_yields_to_the_drop_border() {
    let snap = snapshot();
    let mut i = input(&snap);
    i.tab_count = 2;
    let bars = |g: &DeckGeom| {
        let (prims, hits) = sill_paint(g, &i);
        (prims, hits.iter().map(|h| h.bar()).collect::<Vec<_>>())
    };
    let (prims, tight) = bars(&GEOM_1X);
    assert!(tight.contains(&Bar::Global) && tight.contains(&Bar::Aterm));
    assert!(!tight.contains(&Bar::Tab), "the Tab lane yields");
    // No lane's capsule or groove starts at the Tab lane's row (y 7).
    let tab_row = lane_rows(1.0)[2].0;
    assert!(
        !prims
            .iter()
            .any(|p| matches!(p, DrawPrim::Panel { y, .. } if *y >= tab_row)),
        "nothing is drawn at or below the Tab lane's row"
    );
    let roomy = DeckGeom { h: 9.0, ..GEOM_1X };
    assert!(bars(&roomy).1.contains(&Bar::Tab), "a 9 px card holds it");
    assert!(bars(&GEOM).1.contains(&Bar::Tab), "so does the 2× card");
}

/// The marks span the lanes the card holds, and a count is drawn only where
/// its figures fit: the 2× card counts, the 6 px 1× card shows the marks
/// alone, inside the card.
#[test]
fn the_marks_fit_the_card_and_fold_their_counts_where_they_cannot() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let snap = snapshot();
    let mut i = input(&snap);
    i.marks = vec![
        crate::stats_paint::StatsMark {
            kind: crate::stats_paint::WellMark::Stuck,
            count: 1,
            frac: 1.0,
        },
        crate::stats_paint::StatsMark {
            kind: crate::stats_paint::WellMark::Quiet,
            count: 2,
            frac: 0.5,
        },
    ];
    let texts = |prims: &[DrawPrim]| {
        prims
            .iter()
            .filter(|p| matches!(p, DrawPrim::Text { .. }))
            .count()
    };
    let (wide, _) = sill_paint(&GEOM, &i);
    assert_eq!(texts(&wide), 2, "one count a mark at 2×");
    let (tight, hits) = sill_paint(&GEOM_1X, &i);
    assert_eq!(texts(&tight), 0, "no count fits a 6 px card");
    let well = hits
        .iter()
        .find(|h| h.instrument() == Instrument::Processes)
        .expect("the marks keep their well");
    assert!(well.rect.1 + well.rect.3 <= GEOM_1X.h);
}

fn wedges(prims: &[DrawPrim], ink: Rgb) -> usize {
    let c = rgba(ink, 0xFF);
    prims
        .iter()
        .filter(|p| matches!(p, DrawPrim::Panel { fill, h, .. } if *fill == c && *h == 1.0))
        .count()
}

/// The disk's ◢ follows the model, not a level: a nearly full volume that is
/// calm draws none; time to full under a day draws it in warn; the Disk row's
/// critical state draws it in critical. Folded under 900 pt the column is the
/// ◢ alone, with its hit.
#[test]
fn the_disk_wedge_follows_the_disk_state_and_a_day_to_full() {
    let mut snap = snapshot();
    if let Some(v) = snap.volume.as_mut() {
        v.free = v.total / 20;
    }
    let calm = sill_paint(&GEOM, &input(&snap)).0;
    let inks = input(&snap).inks;
    assert_eq!(wedges(&calm, inks.warn), 0, "95% used and calm: no ◢");
    snap.facts.filling = Some(aterm_vitals::Filling {
        secs: 6.0 * 3600.0,
        rate: 1e6,
        burst: false,
    });
    let filling = sill_paint(&GEOM, &input(&snap)).0;
    assert!(wedges(&filling, inks.warn) > 0, "full in 6 h: ◢ in warn");
    snap.resources[Resource::Disk.index()].state = Some(State::Critical);
    let critical = sill_paint(&GEOM, &input(&snap)).0;
    assert!(wedges(&critical, inks.critical) > 0);
    assert_eq!(wedges(&critical, inks.warn), 0);
    let folded = DeckGeom {
        w: 700.0 * 2.0 + 48.0,
        ..GEOM
    };
    let (prims, hits) = sill_paint(&folded, &input(&snap));
    assert!(wedges(&prims, inks.critical) > 0);
    assert!(hits.iter().any(|h| h.instrument() == Instrument::Disk));
}

/// The scopes' write rates on the disk column's log scale: nothing at or under
/// 64 KiB/s, a quarter-octave step above it, full at 1 GiB/s.
#[test]
fn write_rates_sit_on_a_log_scale_from_64_kib_to_1_gib() {
    assert_eq!(write_level(64.0 * 1024.0), None);
    assert_eq!(write_level(f64::NAN), None);
    let octave = write_level(128.0 * 1024.0).expect("above the floor");
    assert!(
        (octave - 1.0 / 14.0).abs() < 1e-6,
        "one octave of 14: {octave}"
    );
    assert_eq!(write_level(1024.0 * 1024.0 * 1024.0), Some(1.0));
    assert_eq!(write_level(8.0 * 1024.0 * 1024.0 * 1024.0), Some(1.0));
    // 100 KiB/s and 110 KiB/s round to the same quarter octave.
    assert_eq!(write_level(100.0 * 1024.0), write_level(110.0 * 1024.0));
}

/// The aterm and Tab disk lanes are drawn as stripes from the scope's write
/// rate, and only above 64 KiB/s; each answers the pointer as the disk
/// instrument of its bar.
#[test]
fn the_disk_column_stripes_the_scopes_write_rates() {
    let stripes = |rate: f64| {
        let mut snap = snapshot();
        snap.resources[Resource::Disk.index()].aterm.now = Some(rate);
        let mut i = input(&snap);
        i.tab_count = 2;
        let (prims, hits) = sill_paint(&GEOM, &i);
        let lane = hits
            .iter()
            .find(|h| h.instrument() == Instrument::Disk && h.bar() == Bar::Aterm)
            .expect("the aterm disk lane answers the pointer")
            .rect;
        assert!(
            hits.iter()
                .any(|h| h.instrument() == Instrument::Disk && h.bar() == Bar::Tab)
        );
        let ink = rgba(i.inks.aterm, 0xFF);
        prims
            .iter()
            .filter(|p| {
                matches!(p, DrawPrim::Panel { x, w, fill, .. }
                    if *fill == ink && *x >= lane.0 && *x + *w <= lane.0 + lane.2 && *w <= 2.0)
            })
            .count()
    };
    assert_eq!(stripes(32.0 * 1024.0), 0, "32 KiB/s is the groove alone");
    assert!(stripes(40e6) > 4, "40 MB/s is a striped run");
}

/// Swap's dashed capsule stays in the gap after the memory column: it never
/// reaches the disk column, however much swap is in use.
#[test]
fn the_swap_capsule_stays_in_the_gap_before_the_disk_column() {
    let mut snap = snapshot();
    if let Some(m) = snap.mem.as_mut() {
        m.sample.swap_used = Some(64 << 30);
    }
    let i = input(&snap);
    let (prims, hits) = sill_paint(&GEOM, &i);
    let disk_x = hits
        .iter()
        .find(|h| h.instrument() == Instrument::Disk)
        .expect("the disk column")
        .rect
        .0;
    let mem_end = hits
        .iter()
        .filter(|h| h.instrument() == Instrument::Memory)
        .map(|h| h.rect.0 + h.rect.2)
        .fold(0.0f32, f32::max);
    let swap = a(i.inks.warn, 0.9);
    let dots: Vec<(f32, f32)> = prims
        .iter()
        .filter_map(|p| match p {
            DrawPrim::Panel { x, w, fill, .. } if *fill == swap => Some((*x, *x + *w)),
            _ => None,
        })
        .collect();
    assert!(!dots.is_empty(), "swap in use draws its capsule");
    for (x0, x1) in dots {
        assert!(
            x0 > mem_end && x1 < disk_x,
            "{x0}..{x1} outside the gap {mem_end}..{disk_x}"
        );
    }
    for used in [None, Some(0)] {
        snap.mem.as_mut().unwrap().sample.swap_used = used;
        let (prims, _) = sill_paint(&GEOM, &input(&snap));
        assert!(
            !prims
                .iter()
                .any(|p| matches!(p, DrawPrim::Panel { fill, .. } if *fill == swap)),
            "{used:?}: no measured swap in use draws no capsule"
        );
    }
}

/// A window the gallery draws the Sill in.
#[derive(Clone, Copy, Debug)]
struct SillWindow {
    /// Its width, pt.
    w_pt: f32,
    /// Its pad, pt: the bottom pad the Sill is drawn in, and the side pad.
    pad_pt: f32,
    /// Device px per point.
    scale: f32,
    /// How many tabs it holds.
    tabs: usize,
}

impl SillWindow {
    fn px(self, pt: f32) -> usize {
        (pt * self.scale).round() as usize
    }

    /// The Sill's height in px, 0 where the pad holds none (`App::deck_frame`'s Sill arm).
    fn sill(self) -> usize {
        sill_px(self.px(self.pad_pt), f64::from(self.scale))
    }

    /// The card's height in px. The window is as tall as it is wide, so its drop border is
    /// the widest a window of this width has and the card the shortest: 18 px of a 24 px pad
    /// at 2× from 600 pt of width up, 6 px of a 12 px pad at 1× from 1200 pt up, and more
    /// under those widths, where the border is thinner.
    fn card(self) -> usize {
        let w = self.px(self.w_pt);
        sill_card_px(self.sill(), w, w)
    }

    /// The card as `App::deck_frame` lays it: the frame's width, the card's height, the
    /// window's pad, the terminal's 13 pt font.
    fn geom(self) -> DeckGeom {
        DeckGeom {
            w: self.px(self.w_pt) as f32,
            h: self.card() as f32,
            pad: self.px(self.pad_pt) as f32,
            scale: self.scale,
            font_px: 13.0 * self.scale,
        }
    }
}

/// One Sill frame: the window, the ground it is drawn on, and what [`sill_paint`] draws and
/// answers in it.
struct SillFrame {
    window: SillWindow,
    ground: Rgb,
    prims: Vec<DrawPrim>,
    hits: Vec<Hit>,
}

impl SillFrame {
    /// The frame as the window's foot shows it: the whole bottom pad on its ground. The card
    /// is rastered at its own height, as `App::splice_stats_deck` rasters it, so what the
    /// Sill draws past the card is cut as the window cuts it; the rows the drop border keeps
    /// under the card are the ground.
    fn raster(&self) -> (Vec<u8>, u32, u32) {
        let [r, g, b] = self.ground;
        let pad = self.window.px(self.window.pad_pt) as u32;
        let (mut rgba8, pw, card) = crate::tray_raster::rasterize_tray(
            &self.prims,
            self.window.px(self.window.w_pt) as u32,
            self.window.card() as u32,
            1.0,
            [r, g, b, 0xFF],
        );
        let under = pad.saturating_sub(card);
        rgba8.extend([r, g, b, 0xFF].repeat((pw * under) as usize));
        (rgba8, pw, card + under)
    }
}

/// Owner scene `o` as the Sill reads it. [`owner_scene`] fills the deck's rows
/// (`Snapshot::resources`) and leaves the scope views the Sill's lanes read (the machine's
/// and aterm's, the front tab's, and the CPU's busy cores) at their defaults, which draw
/// every scene's CPU lanes as empty grooves; this fills them from the same rows' figures,
/// so the Sill says what the band over it says. Each is a still reading (a flat trend), as
/// the scenes' cores are: no lane rises or falls, so no frame draws a ghost or a glow.
fn sill_scene(o: Owner) -> Snapshot {
    let mut snap = owner_scene(o);
    let cpu = snap.resource(Resource::Cpu, &[FRONT]);
    let mem = snap.resource(Resource::Memory, &[FRONT]);
    let scope = |cores: Option<f64>, bytes: Option<f64>| ScopeView {
        cores: TrendView::flat(cores.unwrap_or(0.0) as f32),
        footprint: bytes.map(|b| b as u64),
        ..ScopeView::default()
    };
    let busy = TrendView::flat(cpu.now.unwrap_or(0.0) as f32);
    snap.global.cores = busy;
    if let Some(c) = snap.cpu.as_mut() {
        c.busy = busy;
    }
    snap.aterm = Some(scope(cpu.aterm.now, mem.aterm.now));
    for tab in snap.tabs.iter_mut().filter(|t| t.key == FRONT) {
        tab.scope = scope(cpu.tab.now, mem.tab.now);
    }
    snap
}

/// The Sill `window` shows for `snap` on `inks`; `None` where it shows none, as
/// `App::deck_frame`'s Sill arm gives no frame for a pad under 8 pt or a card of no height.
fn sill_frame(snap: &Snapshot, inks: VitalsInks, window: SillWindow) -> Option<SillFrame> {
    if window.sill() == 0 || window.card() == 0 {
        return None;
    }
    Some(sill_frame_at(snap, inks, window, window.geom()))
}

/// What [`sill_paint`] draws for `window` at `geom`, whatever the window's pad holds.
fn sill_frame_at(
    snap: &Snapshot,
    inks: VitalsInks,
    window: SillWindow,
    geom: DeckGeom,
) -> SillFrame {
    let input = DeckInput {
        inks,
        tab_count: window.tabs,
        ..owner_input(snap)
    };
    let (prims, hits) = sill_paint(&geom, &input);
    SillFrame {
        window,
        ground: inks.ground,
        prims,
        hits,
    }
}

/// Where each panel and stroke of `prims` is inked, as `(index, [x0, y0, x1, y1])`: a panel's
/// rect, and the px a stroke's line is drawn in ([`crate::tray_raster::stroke_px`]: a rounded
/// stroke's line is centred on its rect's edge, so half of it falls outside the rect), each
/// cut to the clips around it. One its clips cut away whole is left out.
fn inked(prims: &[DrawPrim]) -> Vec<(usize, [f32; 4])> {
    let mut clips: Vec<[f32; 4]> = Vec::new();
    let mut out = Vec::new();
    for (i, p) in prims.iter().enumerate() {
        let rect = match *p {
            DrawPrim::ClipPush { x, y, w, h } => {
                clips.push([x, y, x + w, y + h]);
                continue;
            }
            DrawPrim::ClipPop => {
                clips.pop();
                continue;
            }
            DrawPrim::Panel { x, y, w, h, .. } => [x, y, x + w, y + h],
            DrawPrim::Stroke {
                x,
                y,
                w,
                h,
                radius,
                width,
                ..
            } => {
                let (x, y, w, h) = crate::tray_raster::stroke_px((x, y, w, h), radius, width);
                [x, y, x + w, y + h]
            }
            _ => continue,
        };
        let cut = clips.iter().fold(rect, |r, c| {
            [
                r[0].max(c[0]),
                r[1].max(c[1]),
                r[2].min(c[2]),
                r[3].min(c[3]),
            ]
        });
        if cut[0] < cut[2] && cut[1] < cut[3] {
            out.push((i, cut));
        }
    }
    out
}

/// What a Sill frame must show, as the faults `f` has for the scene `snap` (none: it holds).
/// Each fault starts with the law it breaks:
///
/// - `PAD`: a frame exists only where the pad holds a Sill ([`sill_px`] and
///   [`sill_card_px`] are not 0); a frame that has this fault is judged no further.
/// - `KIND`: the Sill draws panels, strokes and clips, which the `CARD` law places.
/// - `CLIP`: each clip the Sill opens it closes.
/// - `CARD`: no panel or stroke is inked outside the card ([`inked`]), a rounded rim's line
///   included, half of which falls outside its rect: the GPU tank's rim keeps its top line
///   and its bottom line on the card's rows at 1× as at 2×. And no region that answers the
///   pointer reaches past the frame's sides: a folded column's pressure cap and ◢, with
///   their regions, are in the frame like everything else. Nothing is let past.
/// - `TAB`: the Tab lane draws, and answers the pointer in each column the frame has, only
///   with two or more tabs and only where the card holds its row; elsewhere the columns
///   place nothing in that row.
/// - `FOLD`: the columns present follow [`Fold::of`] at the Sill's width: the CPU column,
///   the memory and disk columns' lanes where the law keeps them, and where it folds one,
///   only its pressure cap or its ◢, and only while the scene has one; and each region
///   that answers the pointer holds something drawn. A minimal Sill (under 240 pt) is not
///   judged: it is a fault to ask.
fn sill_faults(snap: &Snapshot, f: &SillFrame) -> Vec<String> {
    let win = f.window;
    let (sill, card) = (win.sill(), win.card());
    if sill == 0 || card == 0 {
        return vec![format!(
            "PAD: a frame for a {} pt pad at {}x, which holds no Sill (sill {sill} px, card \
             {card} px)",
            win.pad_pt, win.scale
        )];
    }
    let mut faults = Vec::new();
    let (w, card_h) = (win.px(win.w_pt) as f32, card as f32);
    let boxes = inked(&f.prims);

    for p in &f.prims {
        if !matches!(
            p,
            DrawPrim::Panel { .. }
                | DrawPrim::Stroke { .. }
                | DrawPrim::ClipPush { .. }
                | DrawPrim::ClipPop
        ) {
            faults.push(format!("KIND: the law does not place {p:?}"));
        }
    }
    let (mut open, mut unopened) = (0usize, 0usize);
    for p in &f.prims {
        match p {
            DrawPrim::ClipPush { .. } => open += 1,
            DrawPrim::ClipPop if open == 0 => unopened += 1,
            DrawPrim::ClipPop => open -= 1,
            _ => {}
        }
    }
    if open > 0 || unopened > 0 {
        faults.push(format!(
            "CLIP: {open} clip(s) left open, {unopened} closed that none opened"
        ));
    }

    let at = |i: Instrument, bar: Bar| {
        f.hits
            .iter()
            .filter(|h| h.instrument() == i && h.bar() == bar)
            .count()
    };
    let pad = win.px(win.pad_pt) as f32;
    let fold = Fold::of(((w - pad).max(pad) - pad) / win.scale.max(0.5));
    for &(i, [x0, y0, x1, y1]) in &boxes {
        if x0 < 0.0 || y0 < 0.0 || y1 > card_h || x1 > w {
            faults.push(format!(
                "CARD: prim {i} is inked at x {x0}..{x1}, y {y0}..{y1}, outside the {w} x \
                 {card_h} px card: {:?}",
                f.prims[i]
            ));
        }
    }
    for h in &f.hits {
        let (x, _, hw, _) = h.rect;
        if x < 0.0 || x + hw > w {
            faults.push(format!(
                "CARD: the {:?} {:?} region at x {x}..{} is outside the {w} px frame",
                h.instrument(),
                h.bar(),
                x + hw
            ));
        }
    }

    let rows = lane_rows(win.scale.max(0.5));
    let tab_top = rows[2].0;
    let lit = win.tabs >= 2 && tab_top + rows[2].1 <= card_h;
    for i in [Instrument::Cpu, Instrument::Memory, Instrument::Disk] {
        let want = usize::from(lit && at(i, Bar::Aterm) > 0);
        if at(i, Bar::Tab) != want {
            faults.push(format!(
                "TAB: {} Tab lane(s) answer on {i:?}, not {want}: {} tab(s), a {card_h} px \
                 card, the Tab lane's row at {tab_top}..{}",
                at(i, Bar::Tab),
                win.tabs,
                tab_top + rows[2].1
            ));
        }
    }
    // The lanes' columns start at the CPU column, right of the core row and the GPU tank,
    // which are taller than a lane and cross the Tab lane's row.
    let columns = f
        .hits
        .iter()
        .find(|h| h.instrument() == Instrument::Cpu && h.bar() == Bar::Aterm)
        .map(|h| h.rect.0);
    if let Some(columns) = columns {
        let in_row = boxes
            .iter()
            .filter(|(_, b)| b[0] >= columns - 1.0 && b[3] > tab_top)
            .count();
        if lit != (in_row > 0) {
            faults.push(format!(
                "TAB: {in_row} prim(s) placed in the Tab lane's row under y {tab_top} with {} \
                 tab(s) on a {card_h} px card",
                win.tabs
            ));
        }
    }

    if fold.minimal {
        faults.push(format!("FOLD: a minimal Sill is not judged: {fold:?}"));
        return faults;
    }
    // What the scene has to show in a folded column: memory pressure at warn or above, and
    // a volume that wants a look (the Disk row's state, or a day or less to full).
    let pressed = snap.mem.as_ref().is_some_and(|m| {
        matches!(
            m.sample.pressure,
            aterm_vitals::Pressure::Warn | aterm_vitals::Pressure::Critical
        )
    });
    let wedge = snap.volume.is_some()
        && (snap.resources[Resource::Disk.index()].state.is_some() || snap.facts.filling.is_some());
    let memory = fold.memory && snap.mem.is_some();
    let disk = fold.disk && snap.volume.is_some();
    let want = [
        (Instrument::Cpu, Bar::Aterm, 1),
        (Instrument::Memory, Bar::Aterm, usize::from(memory)),
        (
            Instrument::Memory,
            Bar::Global,
            usize::from(memory || pressed),
        ),
        (Instrument::Disk, Bar::Aterm, usize::from(disk)),
        (Instrument::Disk, Bar::Global, usize::from(disk || wedge)),
    ];
    for (i, bar, n) in want {
        if at(i, bar) != n {
            faults.push(format!(
                "FOLD: {} {i:?} {bar:?} region(s), not {n}, at {fold:?} (pressure cap \
                 {pressed}, disk wedge {wedge})",
                at(i, bar)
            ));
        }
    }
    for h in &f.hits {
        let (x, y, hw, hh) = h.rect;
        let drawn = boxes
            .iter()
            .any(|(_, b)| b[0] < x + hw && b[2] > x && b[1] < y + hh && b[3] > y);
        if !drawn {
            faults.push(format!(
                "FOLD: the {:?} {:?} region at x {x}..{} holds nothing drawn",
                h.instrument(),
                h.bar(),
                x + hw
            ));
        }
    }
    faults
}

/// The region of the folded disk's ◢ in `f`.
fn wedge_region(f: &mut SillFrame) -> &mut (f32, f32, f32, f32) {
    &mut f
        .hits
        .iter_mut()
        .find(|h| h.instrument() == Instrument::Disk && h.bar() == Bar::Global)
        .expect("the folded disk's ◢")
        .rect
}

/// The laws each fault of `faults` names, in order, once each.
fn laws(faults: &[String]) -> Vec<&str> {
    let mut laws: Vec<&str> = faults
        .iter()
        .map(|f| f.split(':').next().unwrap_or_default())
        .collect();
    laws.dedup();
    laws
}

/// The Sill for a person to read, and what its frames must show ([`sill_faults`]). Each of
/// the six owner scenes is drawn in windows 1440, 1039, 700 and 450 pt wide, at 2× and 1×,
/// on the dark and the light ground, with one tab and with three: a 12 pt pad, and the card
/// a window as tall as it is wide keeps of it ([`SillWindow::card`]). Each frame is the
/// window's whole bottom pad: the card as the window rasters it, over the rows the drop
/// border keeps. The grounds are the deck fixtures' ([`dark_inks`], [`light_inks`], as
/// `deck_gallery` draws the band); a window inks its Sill for the terminal's floor
/// (`App::sill_floor`). One more frame a scene, at the owner's 1039 pt window and 2×, stacks
/// the Folded band over the Sill, a pad of ground between them: what the window that
/// carries the band shows, over what a window without it keeps. A 7 pt pad holds no Sill
/// and writes no frame.
///
/// `ATERM_DECK_GALLERY=<dir>` keeps the frames.
#[test]
fn the_sill_gallery_keeps_every_scene_inside_its_card_at_both_scales() {
    const WIDTHS: [f32; 4] = [1440.0, OWNER_W as f32, 700.0, 450.0];
    const SCALES: [f32; 2] = [2.0, 1.0];
    const TABS: [usize; 2] = [1, 3];
    const PAD_PT: f32 = 12.0;
    let (dir, _gallery) = crate::scripted_redraw_fixture::gallery_directory("ATERM_DECK_GALLERY");
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let grounds = [("dark", dark_inks()), ("light", light_inks())];
    let png = |rgba8: &[u8], pw: u32, ph: u32| {
        crate::app_introspect::encode_rgba8_png(rgba8, pw, ph).expect("the frame encodes")
    };
    let mut frames = std::collections::BTreeSet::new();
    let mut stacked = std::collections::BTreeSet::new();
    // Which arms of the law the gallery's own frames reach.
    let (mut lit, mut yielded, mut folds) = (0, 0, Vec::new());
    let (mut caps, mut wedges) = (0, 0);
    for o in Owner::ALL {
        let snap = sill_scene(o);
        for w_pt in WIDTHS {
            for scale in SCALES {
                for (ground, inks) in grounds {
                    for tabs in TABS {
                        let window = SillWindow {
                            w_pt,
                            pad_pt: PAD_PT,
                            scale,
                            tabs,
                        };
                        let f = sill_frame(&snap, inks, window).expect("12 pt holds the Sill");
                        let faults = sill_faults(&snap, &f);
                        assert!(
                            faults.is_empty(),
                            "{} {window:?} on {ground}: {faults:#?}",
                            o.name()
                        );
                        let tab_lanes = f.hits.iter().filter(|h| h.bar() == Bar::Tab).count();
                        lit += usize::from(tab_lanes > 0);
                        yielded += usize::from(tabs >= 2 && tab_lanes == 0);
                        // A folded column is its cap or its ◢ alone: a region with no lanes.
                        let alone = |i: Instrument| {
                            let n = |bar: Bar| {
                                f.hits
                                    .iter()
                                    .filter(|h| h.instrument() == i && h.bar() == bar)
                                    .count()
                            };
                            usize::from(n(Bar::Global) == 1 && n(Bar::Aterm) == 0)
                        };
                        caps += alone(Instrument::Memory);
                        wedges += alone(Instrument::Disk);
                        let (rgba8, pw, ph) = f.raster();
                        let file = dir.join(format!(
                            "sill_{}_{w_pt}pt_{scale}x_{ground}_{tabs}tab.png",
                            o.name()
                        ));
                        std::fs::write(&file, png(&rgba8, pw, ph)).expect("the PNG writes");
                        frames.insert(file);
                    }
                }
                let thin = SillWindow {
                    w_pt,
                    pad_pt: 7.0,
                    scale,
                    tabs: 3,
                };
                assert_eq!(thin.sill(), 0, "{thin:?}");
                assert!(
                    sill_frame(&snap, dark_inks(), thin).is_none(),
                    "a 7 pt pad writes no frame: {thin:?}"
                );
            }
            let fold = Fold::of(w_pt - 2.0 * PAD_PT);
            if !folds.contains(&fold) {
                folds.push(fold);
            }
        }

        // The Folded band over the Sill, on the dark ground with three tabs, so the Sill
        // shows its three lanes under the band's three scopes. The band is the deck's own
        // scene at the owner's window, on the ground the Sill under it is drawn on.
        let inks = dark_inks();
        let [r, g, b] = inks.ground;
        let band_scene = owner_scene(o);
        let band_input = DeckInput {
            inks,
            form: DeckForm::Folded,
            tab_count: 3,
            ..owner_input(&band_scene)
        };
        let band_geom = owner_geom_at(DeckForm::Folded, OWNER_W, 2.0);
        let (band, _) = deck_paint(&band_geom, &band_input);
        let (mut rgba8, pw, band_h) = crate::tray_raster::rasterize_tray(
            &band,
            band_geom.w as u32,
            band_geom.h as u32,
            1.0,
            [r, g, b, 0xFF],
        );
        let window = SillWindow {
            w_pt: OWNER_W as f32,
            pad_pt: PAD_PT,
            scale: 2.0,
            tabs: 3,
        };
        let f = sill_frame(&snap, inks, window).expect("12 pt holds the Sill");
        let (sill, sill_w, sill_h) = f.raster();
        assert_eq!(pw, sill_w, "the band and the Sill are one window's width");
        let gap = window.px(PAD_PT) as u32;
        rgba8.extend([r, g, b, 0xFF].repeat((pw * gap) as usize));
        rgba8.extend(sill);
        let file = dir.join(format!(
            "sill_under_band_{}_{OWNER_W}pt_2x_dark_3tab.png",
            o.name()
        ));
        std::fs::write(&file, png(&rgba8, pw, band_h + gap + sill_h)).expect("the PNG writes");
        stacked.insert(file);
    }
    assert_eq!(
        (frames.len(), stacked.len()),
        (192, 6),
        "6 scenes x 4 widths x 2 scales x 2 grounds x 2 tab counts, and a stacked frame a scene"
    );
    assert_eq!(
        frames.len(),
        Owner::ALL.len() * WIDTHS.len() * SCALES.len() * grounds.len() * TABS.len()
    );

    // The frames reach each arm of the law. The 2× pad keeps an 18 px card and the 1× pad a
    // 6 px one in a 1440 pt window; the Tab lane draws in some frames and yields to the drop
    // border in others; the four widths fold to all columns, to no disk column, and to
    // neither it nor the memory column; and some folded column shows its pressure cap alone,
    // some its ◢ alone.
    let at_1440 = |scale: f32, pad_pt: f32| SillWindow {
        w_pt: 1440.0,
        pad_pt,
        scale,
        tabs: 3,
    };
    assert_eq!(at_1440(2.0, PAD_PT).card(), 18);
    assert_eq!(at_1440(1.0, PAD_PT).card(), 6);
    assert!(lit > 0 && yielded > 0, "{lit} lit, {yielded} yielded");
    assert!(caps > 0 && wedges > 0, "{caps} caps, {wedges} wedges");
    assert_eq!(
        folds
            .iter()
            .map(|f| (f.disk, f.memory, f.per_core, f.minimal))
            .collect::<Vec<_>>(),
        [
            (true, true, true, false),
            (false, true, true, false),
            (false, false, true, false)
        ]
    );

    // CONTROLS. The frame the controls spoil holds, and its lanes are inked: the hot-hard
    // scene's CPU column draws aterm's and the tab's cores.
    let snap = sill_scene(Owner::HotHard);
    let inks = dark_inks();
    let window = SillWindow {
        w_pt: OWNER_W as f32,
        pad_pt: PAD_PT,
        scale: 2.0,
        tabs: 3,
    };
    let frame = |window: SillWindow| sill_frame(&snap, inks, window).expect("the Sill draws");
    let good = frame(window);
    assert_eq!(sill_faults(&snap, &good), Vec::<String>::new());
    for ink in [inks.aterm, inks.tab] {
        // A lane's fill is a panel in its own clip; the disk column's stripes have none.
        let fills = |f: &SillFrame| {
            f.prims
                .windows(2)
                .filter(|w| {
                    matches!(w, [DrawPrim::ClipPush { .. }, DrawPrim::Panel { fill, .. }]
                        if *fill == rgba(ink, 0xFF))
                })
                .count()
        };
        assert_eq!(fills(&good), 2, "the CPU and memory lanes in {ink:?}");
        let bare = sill_frame_at(&owner_scene(Owner::HotHard), inks, window, window.geom());
        assert_eq!(
            fills(&bare),
            0,
            "CONTROL: the deck's scene alone leaves the Sill's lanes in {ink:?} grooves"
        );
    }

    // A prim drawn below the card.
    let mut below = frame(window);
    below.prims.push(panel(
        window.px(PAD_PT) as f32,
        window.card() as f32,
        40.0,
        2.0,
        0.0,
        rgba(inks.critical, 0xFF),
    ));
    let faults = sill_faults(&snap, &below);
    assert_eq!(laws(&faults), ["CARD"], "CONTROL: {faults:#?}");

    // A prim the law does not place, inside the card.
    let mut odd = frame(window);
    odd.prims.push(DrawPrim::Dot {
        cx: 40.0,
        cy: 9.0,
        r: 2.0,
        color: rgba(inks.critical, 0xFF),
    });
    let faults = sill_faults(&snap, &odd);
    assert_eq!(laws(&faults), ["KIND"], "CONTROL: {faults:#?}");

    // A clip left open that cuts every column away: the regions still answer, over nothing.
    let mut cut = frame(window);
    cut.prims.insert(
        0,
        DrawPrim::ClipPush {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        },
    );
    let faults = sill_faults(&snap, &cut);
    assert_eq!(
        laws(&faults),
        ["CLIP", "TAB", "FOLD"],
        "CONTROL: {faults:#?}"
    );

    // A Tab lane drawn with one tab.
    let mut one_tab = frame(window);
    one_tab.window.tabs = 1;
    let faults = sill_faults(&snap, &one_tab);
    assert_eq!(laws(&faults), ["TAB"], "CONTROL: {faults:#?}");
    assert_eq!(
        faults.len(),
        4,
        "three columns' lanes and their ink: {faults:#?}"
    );

    // A Tab lane drawn where the card does not hold it: a 9 px card's three 1× lanes (a
    // small window's 3 px border) in the 1440 pt window's 6 px card.
    let tight = at_1440(1.0, PAD_PT);
    let roomy = DeckGeom {
        h: 9.0,
        ..tight.geom()
    };
    let faults = sill_faults(&snap, &sill_frame_at(&snap, inks, tight, roomy));
    assert!(
        laws(&faults).contains(&"TAB") && laws(&faults).contains(&"CARD"),
        "CONTROL: {faults:#?}"
    );

    // A frame written for a 7 pt pad.
    let thin = SillWindow {
        pad_pt: 7.0,
        ..window
    };
    let forced = DeckGeom {
        h: sill_card_px(thin.px(7.0), thin.px(thin.w_pt), thin.px(thin.w_pt)) as f32,
        ..thin.geom()
    };
    assert!(forced.h > 0.0, "CONTROL: the 7 pt pad has room to draw in");
    let faults = sill_faults(&snap, &sill_frame_at(&snap, inks, thin, forced));
    assert_eq!(laws(&faults), ["PAD"], "CONTROL: {faults:#?}");

    // A Sill folded to its memory column in a window whose width keeps the disk column.
    let mut folded = frame(SillWindow {
        w_pt: 700.0,
        ..window
    });
    folded.window.w_pt = OWNER_W as f32;
    let faults = sill_faults(&snap, &folded);
    assert_eq!(laws(&faults), ["FOLD"], "CONTROL: {faults:#?}");

    // A folded disk's ◢ and its region one column gap past the Sill's right end, where the
    // Sill placed them before their slot came off the columns' room: the frame's right edge,
    // since the side pad is that gap's 12 pt. After the memory column (700 pt) and after
    // the pressure cap's slot (450 pt), in the hot-hard scene, whose disk fills.
    let wedge = rgba(
        disk_wedge(&snap, &inks).expect("the hot-hard disk fills"),
        0xFF,
    );
    for w_pt in [700.0, 450.0] {
        let mut past = frame(SillWindow { w_pt, ..window });
        assert_eq!(sill_faults(&snap, &past), Vec::<String>::new(), "{w_pt} pt");
        let px = |pt: f32| past.window.px(pt) as f32;
        let (w, gap) = (px(w_pt), px(COL_GAP_PT));
        let right = w - px(PAD_PT);
        assert_eq!(right + gap, w, "{w_pt} pt");

        // The ◢'s region alone, reaching a px past the frame's right edge over its ◢.
        let mut wide = frame(SillWindow { w_pt, ..window });
        let region = wedge_region(&mut wide);
        region.2 = w + 1.0 - region.0;
        let faults = sill_faults(&snap, &wide);
        assert_eq!(laws(&faults), ["CARD"], "CONTROL: {w_pt} pt: {faults:#?}");
        assert_eq!(faults.len(), 1, "CONTROL: the region alone: {faults:#?}");

        // The ◢ and its region where the Sill placed them.
        let region = wedge_region(&mut past);
        let (from, slot) = (region.0, region.2);
        let dx = right + gap - from;
        assert!(dx > 0.0, "{w_pt} pt: the ◢ is inside the Sill's right end");
        region.0 += dx;
        for p in &mut past.prims {
            if let DrawPrim::Panel { x, fill, .. } = p
                && *fill == wedge
                && (from..from + slot).contains(&*x)
            {
                *x += dx;
            }
        }
        let faults = sill_faults(&snap, &past);
        assert_eq!(laws(&faults), ["CARD"], "CONTROL: {w_pt} pt: {faults:#?}");
    }

    // A 1× tank whose rect ends on the card's last row, as the Sill placed it on a 6 px card
    // before its rim was held on the card: the rim's bottom line is under the card. And one
    // whose rect starts on the card's first row: its top line is over the card.
    let tight = at_1440(1.0, PAD_PT);
    let card_h = tight.card() as f32;
    for (y, h) in [(1.0, card_h - 1.0), (0.0, card_h - 1.0)] {
        let mut tank = frame(tight);
        let (gx, _, gw, _) = tank
            .hits
            .iter()
            .find(|h| h.instrument() == Instrument::Gpu)
            .expect("the GPU tank")
            .rect;
        let mut p = Painter::new(&inks, 1.0, tight.geom().font_px, false);
        p.tank((gx + 1.0, y, gw - 2.0, h), Mark::still(0.5), None, 2.0);
        tank.prims.extend(p.prims);
        let faults = sill_faults(&snap, &tank);
        assert_eq!(
            laws(&faults),
            ["CARD"],
            "CONTROL: a tank at y {y}..{}: {faults:#?}",
            y + h
        );
    }
}
