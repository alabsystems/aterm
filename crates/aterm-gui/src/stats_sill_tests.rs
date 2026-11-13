// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::stats_deck::tests::{input, snapshot};

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
        m.sample.swap_used = 64 << 30;
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
}
