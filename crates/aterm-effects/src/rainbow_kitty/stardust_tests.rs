// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::Flow;
use super::super::spine::FLOW_ENTRY_KEYS;
use super::*;
use crate::rainbow_kitty::Mend;
use crate::rainbow_kitty::ribbon::RETRACT_START_S;
use aterm_render::add_sat;
use std::time::Duration;

const fn geom() -> Geom {
    Geom {
        cw: 9,
        ch: 18,
        rows: 40,
        cols: 120,
        origin_x: 0,
        origin_y: 40,
        win_w: 1080,
        win_h: 760,
        head: 40,
    }
}

const fn cfg(dark: bool) -> Config {
    Config {
        dark_theme: dark,
        intensity: 1.0,
        duration: Duration::from_millis(900),
        ribbon_tall: true,
        ribbon_flat: false,
        theme_fg: if dark { 0x00FF_FFFF } else { 0x0016_161C },
        theme_bg: if dark { 0x0000_0000 } else { 0x00FF_FFFF },
        reduced_motion: false,
        light: crate::cursor_glow::Light::OFF,
        head_dx: 0.5,
    }
}

const fn geom_cell(cw: usize, ch: usize) -> Geom {
    Geom {
        cw,
        ch,
        win_w: (120 * cw) as u16,
        win_h: (40 + 40 * ch) as u16,
        ..geom()
    }
}

const fn ctx(now: Instant, cfg: &Config, caret: (u16, u16)) -> Ctx<'_> {
    ctx_in(geom(), now, cfg, caret)
}

const fn ctx_in(geom: Geom, now: Instant, cfg: &Config, caret: (u16, u16)) -> Ctx<'_> {
    Ctx {
        now,
        geom,
        cfg,
        disp: 0.4,
        birth_disp: 0.4,
        phase: 0.0,
        caret,
        caret_t: 0.5,
        caret_walk: None,
        mend: None,
        surge: 0.0,
        flow: Flow::EMPTY,
    }
}

#[derive(Default)]
struct Scratch {
    under: Vec<GlowQuad>,
    out: Vec<GlowQuad>,
    under_ramps: aterm_render::GlowRamps,
    out_ramps: aterm_render::GlowRamps,
    halos: Vec<RainHalo>,
    beams: Vec<aterm_render::BeamVertex>,
    cues: Vec<SoundCue>,
}

impl Scratch {
    const fn frame(&mut self) -> Frame<'_> {
        Frame {
            under: &mut self.under,
            out: &mut self.out,
            under_ramps: &mut self.under_ramps,
            out_ramps: &mut self.out_ramps,
            halos: &mut self.halos,
            beams: &mut self.beams,
            cues: &mut self.cues,
            caret: super::super::CaretSeam::EMPTY,
            companion: None,
            fp: 0,
            rows_cost: Some(aterm_render::RampCost::NEVER),
        }
    }
}

fn px(quads: &[GlowQuad], x: i32, y: i32) -> u32 {
    let mut dst = 0u32;
    for q in quads {
        let inside = x >= i32::from(q.x)
            && x < i32::from(q.x) + i32::from(q.w)
            && y >= i32::from(q.y)
            && y < i32::from(q.y) + i32::from(q.h);
        if !inside {
            continue;
        }
        if q.alpha == 0 {
            dst = add_sat(dst, q.color);
        } else {
            let a = u32::from(q.alpha);
            let ch = |sh: u32| {
                let d = (dst >> sh) & 0xff;
                (((q.color >> sh) & 0xff) + d * (255 - a) / 255).min(255)
            };
            dst = (ch(16) << 16) | (ch(8) << 8) | ch(0);
        }
    }
    dst
}

const fn lum(p: u32) -> u32 {
    let r = (p >> 16) & 0xff;
    let g = (p >> 8) & 0xff;
    let b = p & 0xff;
    let rg = if r >= g { r } else { g };
    if rg >= b { rg } else { b }
}

fn peak_seed() -> u32 {
    (0u32..)
        .find(|&k| {
            (hash01(mix32(k ^ SALT_PHASE)) * std::f32::consts::TAU)
                .sin()
                .abs()
                > 0.9995
        })
        .expect("some seed puts the luminance clock at its peak")
}

fn star(class: StarClass, lane: StarLane, born: Instant, tint: u32) -> Star {
    Star {
        class,
        lane,
        x: 200.0,
        y: 100.0,
        home_row: None,
        born,
        lit: born,
        seed: peak_seed(),
        tint,
        gold: tint == TINT_GOLD_RGB,
        v0: (0.0, 0.0),
        f: 10.0,
        stream: 0.0,
        pull: 0.0,
        finish: None,
        beside_caret: None,
        edge_cut: false,
    }
}

fn sky() -> Stardust {
    let mut sky = Stardust::new();
    for row in 2..=4 {
        sky.probe_mut().probe_row(row, &[false; 120]);
    }
    sky
}

fn frame_at(sky: &mut Stardust, now: Instant, cfg: &Config) -> Scratch {
    frame_in(sky, geom(), now, cfg)
}

fn frame_in(sky: &mut Stardust, g: Geom, now: Instant, cfg: &Config) -> Scratch {
    let mut sc = Scratch::default();
    let mut f = sc.frame();
    sky.emit(&ctx_in(g, now, cfg, (3, 10)), &mut f);
    sc
}

/// A star whose centre lies on the pet's body, or on the box the body's last move carries it
/// to, is not drawn on dark or on paper; one beside the body is. The negative control notes no
/// body and draws it.
#[test]
fn no_star_is_drawn_on_the_pet_s_body() {
    let t0 = Instant::now();
    // The star sits at (200, 100); the body is 70 px wide and 44 tall.
    let body = |x0: i32| Some((x0, x0 + 70, 70, 114));
    for dark in [true, false] {
        let c = cfg(dark);
        let drawn = |boxes: &[Option<(i32, i32, i32, i32)>]| {
            let mut sky = sky();
            sky.sow(star(StarClass::M1, StarLane::Strike, t0, TINT_WHITE_RGB));
            for &b in boxes {
                sky.shade.note(b);
            }
            let sc = frame_at(&mut sky, t0 + Duration::from_millis(40), &c);
            sc.out.len() + sc.halos.len()
        };
        assert!(drawn(&[]) > 0, "dark={dark}: the control draws the star");
        assert!(
            drawn(&[None]) > 0,
            "dark={dark}: a pet off glass shades nothing"
        );
        assert_eq!(
            drawn(&[body(170)]),
            0,
            "dark={dark}: the star is on the body"
        );
        assert!(
            drawn(&[body(230)]) > 0,
            "dark={dark}: a star beside the body is drawn"
        );
        assert_eq!(
            drawn(&[body(60), body(115)]),
            0,
            "dark={dark}: the body moving onto the star shades it a frame early"
        );
        assert!(
            drawn(&[body(170), None]) > 0,
            "dark={dark}: the body gone, the star is back"
        );
    }
}

const fn every_class() -> [(StarClass, StarLane, f32); 6] {
    [
        (StarClass::M1, StarLane::Strike, 143.0),
        (StarClass::M2, StarLane::Strike, 130.0),
        (StarClass::M3, StarLane::Strike, 98.0),
        (StarClass::M1, StarLane::Fan, 118.0),
        (StarClass::M2, StarLane::Fan, 80.0),
        (StarClass::M3, StarLane::Fan, 61.0),
    ]
}

const fn typed(class: TypedClass) -> Event {
    Event::Typed {
        cells: 1,
        shifted: matches!(class, TypedClass::Capital),
        class,
    }
}

const fn col_of(s: &Star) -> i32 {
    px_col(s.x, geom())
}

fn key_at(t0: Instant, k: u16) -> Instant {
    t0 + Duration::from_micros(u64::from(k) * 83_333)
}

fn disp_at_12cps(k: u16) -> f32 {
    let t = f32::from(k) / 12.0;
    (1.3 * (1.0 - (-t / 2.0).exp())).min(1.0)
}

fn chroma(p: u32) -> u32 {
    let (r, g, b) = ((p >> 16) & 0xff, (p >> 8) & 0xff, p & 0xff);
    r.max(g).max(b) - r.min(g).min(b)
}

#[test]
fn a_grain_opens_and_closes_at_least_twice_in_its_life() {
    let t0 = Instant::now();
    for dark in [true, false] {
        let c = cfg(dark);
        let g = if dark { geom() } else { geom_cell(15, 28) };
        let mut sky = sky();
        sky.sow(star(StarClass::M3, StarLane::Strike, t0, TINT_WHITE_RGB));
        let life_ms = (StarClass::M3.life_s(StarLane::Strike) * 1000.0) as u64;
        let mut widths = Vec::new();
        for ms in 0..life_ms {
            let sc = frame_in(&mut sky, g, t0 + Duration::from_millis(ms), &c);
            let w = if dark {
                sc.out.iter().map(|q| q.w).max().unwrap_or(0)
            } else {
                let row = composite_over(&sc, PAPER, 180, 100, 40, 1);
                let inked: Vec<_> = (0..40u16)
                    .filter(|&x| row[usize::from(x)] != PAPER)
                    .collect();
                inked.last().map_or(0, |last| last - inked[0] + 1)
            };
            if widths.last() != Some(&w) {
                widths.push(w);
            }
        }
        assert!(
            widths.len() >= 5,
            "the {} grain changed size {} times in {life_ms} ms: {widths:?}",
            if dark { "dark" } else { "light" },
            widths.len() - 1
        );
        let widest = i32::from(widths.iter().copied().max().unwrap_or(0));
        let at_2x = if dark { widest * 2 } else { widest };
        assert!(
            at_2x <= SCINT_BBOX_MAX_PX,
            "the {} grain's extent {at_2x} px at 2x is outside the size-scintillation \
             exemption",
            if dark { "dark" } else { "light" }
        );
    }
}

#[test]
fn a_light_rule_over_the_typing_row_keeps_the_sky_alive() {
    let c = cfg(true);
    let t0 = Instant::now();
    let ribbon = Ribbon::new();
    let g = geom();
    let ch = g.ch as f32;
    let type_under = |row_4: &[CellInk]| -> Stardust {
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row_ink(4, row_4);
        sky.probe_mut().probe_row(5, &[false; 120]);
        for k in 0..40u16 {
            sky.on_event(
                &typed(TypedClass::Glyph),
                t0,
                &ctx(t0, &c, (5, 10 + k)),
                &ribbon,
            );
        }
        sky
    };
    let row_of = |s: &str| -> Vec<CellInk> { s.repeat(120).chars().map(cell_ink).collect() };

    let rule = row_of("─");
    assert!(rule.iter().all(|&cell| cell == CellInk::Rule));
    let sky = type_under(&rule);
    assert!(
        sky.live() > 0,
        "a light rule above the input line darkened the sky"
    );
    for s in sky.live_iter() {
        assert_eq!(
            px_row(s.y, g),
            4,
            "a sky star was born outside row − 1, at ({}, {})",
            s.x,
            s.y
        );
        let off = s.y as i32 - i32::from(g.origin_y) - 4 * g.ch as i32;
        assert!(
            off >= rule_stroke_px(g.ch).end,
            "a star core sits in the rule's stroke band: pixel {off} of row 4, {:.2} ch",
            off as f32 / ch
        );
        assert_eq!(
            sky.probe().at_px(s.x as i32, s.y as i32, g),
            Some(false),
            "the pixel probe called a sky pixel over the rule inked at ({}, {})",
            s.x,
            s.y
        );
    }
    let blank = type_under(&row_of(" "));
    assert_eq!(
        sky.live(),
        blank.live(),
        "the sky over a light rule is not the sky over a blank row"
    );

    assert_eq!(
        type_under(&row_of("x")).live(),
        0,
        "a row of text above the input line minted stars"
    );

    assert_eq!(
        type_under(&row_of("━")).live(),
        0,
        "a heavy rule above the input line minted stars"
    );
}

#[test]
fn the_pixel_probe_answers_ink_only_inside_a_rules_stroke_band() {
    let g = geom();
    let mut probe = GlyphProbe::new();
    let mut row = vec![CellInk::Blank; 120];
    row[10] = CellInk::Rule;
    row[11] = CellInk::Glyph;
    probe.probe_row_ink(4, &row);
    let y_at = |f: f32| (f32::from(g.origin_y) + (4.0 + f) * g.ch as f32) as i32;
    let (rx, _) = g.cell_center(4, 10);
    let (gx, _) = g.cell_center(4, 11);
    let (bx, _) = g.cell_center(4, 12);

    assert_eq!(probe.at(4, 10, g.rows), Some(true), "the rule cell has ink");
    assert_eq!(
        probe.sky_at(4, 10, g.rows),
        Some(false),
        "the rule's sky is clear"
    );

    assert_eq!(probe.at(4, 11, g.rows), Some(true));
    assert_eq!(
        probe.sky_at(4, 11, g.rows),
        Some(true),
        "a glyph darkens the sky"
    );
    for f in [0.10, 0.50, 0.80] {
        assert_eq!(
            probe.at_px(gx as i32, y_at(f), g),
            Some(true),
            "a glyph is ink at {f}"
        );
    }

    assert_eq!(probe.at(4, 12, g.rows), Some(false));
    assert_eq!(probe.sky_at(4, 12, g.rows), Some(false));
    assert_eq!(probe.at_px(bx as i32, y_at(0.50), g), Some(false));

    assert_eq!(probe.at(3, 10, g.rows), None, "an unprobed row is unknown");
    assert_eq!(probe.sky_at(3, 10, g.rows), None);
    assert_eq!(probe.at_px(rx as i32, y_at(-0.50), g), None);

    let mut bools = GlyphProbe::new();
    let mut occupied = [false; 120];
    occupied[11] = true;
    bools.probe_row(4, &occupied);
    for col in [10u16, 11, 12] {
        assert_eq!(bools.at(4, col, g.rows), Some(col == 11));
        assert_eq!(bools.sky_at(4, col, g.rows), Some(col == 11));
    }
    probe.probe_row(4, &occupied);
    assert_eq!(probe.at(4, 10, g.rows), Some(false));
    assert_eq!(probe.at_px(rx as i32, y_at(0.50), g), Some(false));
}

#[test]
fn the_stroke_band_is_one_set_of_pixels_for_the_probe_the_zones_and_the_headroom() {
    let t0 = Instant::now();
    let ribbon = Ribbon::new();
    let tall = cfg(true);
    let mut under = cfg(true);
    under.ribbon_tall = false;
    for ch in 6usize..=48 {
        let cw = (ch / 2).max(3);
        let g = geom_cell(cw, ch);
        let band = rule_stroke_px(ch);
        let row_top = |r: i32| i32::from(g.origin_y) + r * ch as i32;
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row_ink(4, &[CellInk::Rule; 120]);
        sky.probe_mut().probe_row_ink(5, &[CellInk::Rule; 120]);
        let (x, _) = g.cell_center(4, 30);
        let x = x as i32;
        assert!(
            band.start < band.end && band.end <= ch as i32,
            "ch {ch}: the band {band:?} is empty or past the cell"
        );
        for r in [4, 5] {
            for off in 0..ch as i32 {
                assert_eq!(
                    sky.probe().at_px(x, row_top(r) + off, g),
                    Some(band.contains(&off)),
                    "ch {ch}: pixel {off} of row {r} against the band {band:?}"
                );
            }
        }
        if ch >= 7 {
            for w in [cw, ch] {
                let light = ((w.min(ch) + 4) / 8).max(1) as i32;
                let s = (ch as i32 - light) / 2;
                assert!(
                    band.start <= s && s + light <= band.end,
                    "ch {ch} cw {w}: the renderer's stroke {s}..{} is not inside the band \
                     {band:?}",
                    s + light
                );
            }
        }
        // Both anchors' skies are in the row above's foot (`sky_birth_px`).
        for (c, row, side) in [(&tall, 4, "tall"), (&under, 4, "underline")] {
            let cx = ctx_in(g, t0, c, (5, 30));
            let mut births = 0usize;
            for seed in 0..256u32 {
                let Some((bx, by)) = sky.sky_birth_px((5, 30), mix32(seed), &cx, &ribbon) else {
                    continue;
                };
                births += 1;
                let off = by as i32 - row_top(row);
                assert!(
                    (0..ch as i32).contains(&off) && !band.contains(&off),
                    "ch {ch} {side}: a birth at pixel {off} of row {row}, in the band {band:?}"
                );
                assert_eq!(
                    sky.probe().at_px(bx as i32, by as i32, g),
                    Some(false),
                    "ch {ch} {side}: a birth at ({bx}, {by}) is on ink by the probe"
                );
            }
            assert!(births > 0, "ch {ch} {side}: no birth under a rule row");
        }
        for off in 0..ch as i32 {
            let y = (row_top(4) + off) as f32;
            let room = sky.rule_headroom_px(30..=30, y, g);
            if off < band.start {
                assert_eq!(
                    room,
                    f32::INFINITY,
                    "ch {ch}: a flight born at pixel {off}, above the band, is clamped"
                );
                continue;
            }
            assert_eq!(
                room,
                (off - band.end).max(0) as f32,
                "ch {ch}: the headroom at pixel {off} is not the pixels to the band's end \
                 {}",
                band.end
            );
            if band.contains(&off) {
                continue;
            }
            let held = y as i32 - room as i32;
            assert_eq!(
                sky.probe().at_px(x, held, g),
                Some(false),
                "ch {ch}: the pixel a flight born at {off} is held at is on ink"
            );
            assert_eq!(
                sky.probe().at_px(x, held - 1, g),
                Some(true),
                "ch {ch}: a flight born at {off} is held a pixel short of the stroke"
            );
        }
    }
}

#[test]
fn the_headroom_under_a_rule_reads_every_column_the_flight_crosses() {
    let g = geom_cell(15, 28);
    let band = rule_stroke_px(g.ch);
    let row_top = i32::from(g.origin_y) + 4 * g.ch as i32;
    let mut sky = Stardust::new();
    let mut title = [CellInk::Blank; 120];
    title[..40].fill(CellInk::Rule);
    sky.probe_mut().probe_row_ink(4, &title);
    sky.probe_mut().probe_row(5, &[false; 120]);
    let y = (row_top + band.end) as f32;
    assert_eq!(
        sky.rule_headroom_px(39..=39, y, g),
        0.0,
        "under the rule's last cell"
    );
    assert_eq!(
        sky.rule_headroom_px(40..=40, y, g),
        f32::INFINITY,
        "under the first blank cell"
    );
    assert_eq!(
        sky.rule_headroom_px(40..=46, y, g),
        f32::INFINITY,
        "a flight over blank cells only"
    );
    assert_eq!(
        sky.rule_headroom_px(39..=46, y, g),
        0.0,
        "one rule cell at the flight's far end is the flight's headroom"
    );
    assert_eq!(
        sky.rule_headroom_px(30..=46, y + 2.0, g),
        2.0,
        "two pixels under the band's end, over a run of rule cells"
    );
    assert_eq!(
        sky.rule_headroom_px(-3..=-1, y, g),
        f32::INFINITY,
        "columns off the grid's left edge bound nothing"
    );
    assert_eq!(
        sky.rule_headroom_px(-3..=0, y, g),
        0.0,
        "…and the first on-grid rule cell beside them does"
    );
    let mut glyphs = Stardust::new();
    glyphs.probe_mut().probe_row(4, &[true; 120]);
    assert_eq!(
        glyphs.rule_headroom_px(0..=46, y, g),
        f32::INFINITY,
        "a row of glyphs has no stroke band: the cell-level gate refuses it, not the headroom"
    );
}

#[test]
fn cell_ink_classifies_the_light_horizontal_rules_and_nothing_else() {
    for ch in ['─', '┄', '┈', '╌', '╴', '╶'] {
        assert_eq!(cell_ink(ch), CellInk::Rule, "{ch:?} (U+{:04X})", ch as u32);
    }
    for ch in [
        '━', '┅', '┉', '╍', '╸', '╺', '╼', '╾', '═', '│', '┃', '┼', '╭', '╮', '▁', '▔', '█', '-',
        '_', '—', '=', '~', 'a', 'A', '0', '\0', '\u{2588}',
    ] {
        assert_eq!(cell_ink(ch), CellInk::Glyph, "{ch:?} (U+{:04X})", ch as u32);
    }
    assert_eq!(cell_ink(' '), CellInk::Blank);
}

#[test]
fn no_star_core_is_ever_born_over_a_probed_glyph() {
    let c = cfg(true);
    let t0 = Instant::now();
    let ribbon = Ribbon::new();

    let mut sky = Stardust::new();
    for k in 0..40u16 {
        sky.on_event(
            &typed(TypedClass::Glyph),
            t0,
            &ctx(t0, &c, (5, 10 + k)),
            &ribbon,
        );
    }
    assert_eq!(sky.live(), 0, "an unprobed sky band minted stars");

    let mut sky = Stardust::new();
    sky.probe_mut().probe_row(4, &[true; 120]);
    for k in 0..40u16 {
        sky.on_event(
            &typed(TypedClass::Glyph),
            t0,
            &ctx(t0, &c, (5, 10 + k)),
            &ribbon,
        );
    }
    assert_eq!(sky.live(), 0, "an inked sky band minted stars");

    let mut sky = Stardust::new();
    sky.probe_mut().probe_row(4, &[false; 120]);
    for k in 0..40u16 {
        sky.on_event(
            &typed(TypedClass::Glyph),
            t0,
            &ctx(t0, &c, (5, 10 + k)),
            &ribbon,
        );
    }
    assert!(sky.live() > 0, "a provably blank sky band minted nothing");

    let mut sky = Stardust::new();
    let mut row6 = [true; 120];
    row6[41] = false;
    sky.probe_mut().probe_row(6, &row6);
    let (ix, iy) = geom().cell_center(6, 40);
    sky.sow_mini_fan(t0, (ix, iy), 3, &ctx(t0, &c, (6, 40)));
    assert_eq!(sky.live(), 3, "a hop onto a glyph minted no mini-fan");
    for s in sky.live_iter() {
        assert_eq!(
            (s.x, s.y),
            (ix.round(), iy.round()),
            "a mini-fan star was not born on the landing pixel"
        );
        let (rx, ry) = s.rest();
        assert_eq!(
            (px_row(ry as f32, geom()), px_col(rx as f32, geom())),
            (6, 40),
            "a mini-fan core came to rest off the landing cell, at ({rx}, {ry})"
        );
    }
}

#[test]
fn a_thrown_core_is_cleared_where_it_rests_nudged_walked_back_or_dropped() {
    let c = cfg(true);
    let t0 = Instant::now();
    let g = geom();
    let mut sky = Stardust::new();
    let mut row6 = [true; 120];
    row6[40] = false;
    sky.probe_mut().probe_row(6, &row6);
    let cx = ctx(t0, &c, (6, 40));
    let window = FAN_THROW_MS;
    let thrown = |dx: i32, dy: i32| {
        let mut s = star(StarClass::M2, StarLane::Fan, t0, TINT_WHITE_RGB);
        s.x = 365.0;
        s.y = 157.0;
        s.v0 = (dx as f32 / window, dy as f32 / window);
        assert_eq!(s.rest(), (365 + dx, 157 + dy), "the fixture's own rest");
        s
    };
    let done = t0 + Duration::from_secs_f32(window / 1000.0) + Duration::from_millis(1);

    let mut s = thrown(7, 12);
    assert!(sky.settle(&mut s, false, &cx));
    assert_eq!(s.rest(), (372, 169), "a clear rest was moved");

    let mut s = thrown(7, 8);
    assert!(sky.settle(&mut s, false, &cx));
    assert_eq!((s.x, s.y), (365.0, 157.0), "a nudge moved the birth");
    assert_eq!(
        s.rest(),
        (372, 166),
        "the rest was not nudged 1 px into the air"
    );
    assert_eq!(
        s.pos(done, false),
        s.rest(),
        "the throw does not end on its rest"
    );

    let mut s = thrown(12, 0);
    assert!(
        !sky.settle(&mut s, false, &cx),
        "a grain whose rest is 12 px into ink was kept"
    );
    let mut s = thrown(12, 0);
    assert!(sky.settle(&mut s, true, &cx));
    assert_eq!((s.x, s.y), (365.0, 157.0), "the walk-back moved the birth");
    assert_eq!(
        s.rest(),
        (368, 157),
        "the walk-back did not stop at the first clear pixel"
    );
    assert_eq!(s.pos(done, false), s.rest());

    let mut s = thrown(0, 0);
    s.lane = StarLane::Shed;
    s.class = StarClass::M3;
    s.x = 374.0;
    assert!(
        !sky.settle(&mut s, false, &cx),
        "a fragment resting on the inked cell it was shed over was kept"
    );

    let mut boxed = Stardust::new();
    boxed.probe_mut().probe_row(5, &[true; 120]);
    boxed.probe_mut().probe_row(6, &row6);
    boxed.probe_mut().probe_row(7, &[true; 120]);
    let (ox, oy) = g.cell_center(6, 40);
    boxed.sow_fan(
        t0,
        FanSow {
            at_px: (ox, oy),
            reach: 23.0,
            walk: LandingWalk::free(0.25),
            seed: 7,
            n: 14,
            hero: true,
        },
        &cx,
    );
    let fan: Vec<&Star> = boxed
        .live_iter()
        .filter(|s| s.lane == StarLane::Fan)
        .collect();
    assert_eq!(
        fan.len(),
        5,
        "boxed in by ink, a landing keeps exactly its hero and four m2, not {}",
        fan.len()
    );
    assert_eq!(
        fan.iter()
            .filter(|s| s.class == StarClass::M1 && s.gold)
            .count(),
        1,
        "the gold hero"
    );
    assert_eq!(
        fan.iter().filter(|s| s.class == StarClass::M2).count(),
        4,
        "its four m2"
    );
    for s in &fan {
        assert_eq!((s.x, s.y), (ox.round(), oy.round()), "born on the landing");
        let (rx, ry) = s.rest();
        assert_eq!(
            (px_row(ry as f32, g), px_col(rx as f32, g)),
            (6, 40),
            "a {:?} came to rest off the landing cell, at ({rx}, {ry})",
            s.class
        );
        assert_ne!(
            (rx, ry),
            (ox.round() as i32, oy.round() as i32),
            "a {:?} was pinned at the centre instead of thrown to the cell's edge",
            s.class
        );
    }
}

#[test]
fn a_transient_over_ink_or_unknown_is_held_to_a_third() {
    let c = cfg(true);
    let t0 = Instant::now();
    let peak = |probe: Option<bool>| {
        let mut sky = Stardust::new();
        if let Some(inked) = probe {
            sky.probe_mut().probe_row(3, &[inked; 120]);
        }
        let mut s = star(StarClass::M2, StarLane::Fan, t0, TINT_WHITE_RGB);
        s.seed = peak_seed();
        sky.sow(s);
        let sc = frame_at(&mut sky, t0, &c);
        lum(px(&sc.out, 200, 100))
    };
    let clear = peak(Some(false));
    let inked = peak(Some(true));
    let unknown = peak(None);
    assert!(
        clear >= 79,
        "the clear-row fan m2 did not reach its 80: {clear}"
    );
    assert!(
        inked * 3 <= clear + 3 && inked > 0,
        "over ink the star is {inked}, not a third of {clear}"
    );
    assert!(
        unknown * 3 <= clear + 3 && unknown > 0,
        "over an unprobed row the star is {unknown}, not a third of {clear}"
    );
}

fn strike_trace(c: &Config, t0: Instant, after_sow: impl FnOnce(&mut GlyphProbe)) -> Vec<u32> {
    let mut sky = sky();
    sky.sow(star(StarClass::M1, StarLane::Strike, t0, TINT_WHITE_RGB));
    after_sow(sky.probe_mut());
    let life_ms = (StarClass::M1.life_s(StarLane::Strike) * 1000.0) as u64;
    (0..life_ms)
        .map(|ms| {
            let sc = frame_at(&mut sky, t0 + Duration::from_millis(ms), c);
            lum(px(&sc.out, 200, 100))
        })
        .collect()
}

#[test]
fn a_sky_star_keeps_its_centre_when_the_probe_forgets_its_row() {
    let t0 = Instant::now();
    let tall = cfg(true);
    let mut underline = cfg(true);
    underline.ribbon_tall = false;
    type Case<'a> = (&'a str, &'a Config, fn(&mut GlyphProbe));
    let cases: [Case<'_>; 2] = [
        ("its row forgotten", &tall, |probe| {
            for row in 20..=22 {
                probe.probe_row(row, &[false; 120]);
            }
            assert!(
                probe.at_px(200, 100, geom()).is_none(),
                "the fixture did not forget the star's row"
            );
        }),
        ("its own cell inked under underline", &underline, |probe| {
            let mut row = [false; 120];
            row[px_col(200.0, geom()) as usize] = true;
            probe.probe_row(3, &row);
            assert_eq!(
                probe.at_px(200, 100, geom()),
                Some(true),
                "the fixture did not ink the star's own cell"
            );
        }),
    ];
    for (label, c, change) in cases {
        let before = strike_trace(c, t0, |_| {});
        let after = strike_trace(c, t0, change);
        let peak = before.iter().copied().max().unwrap_or(0);
        assert!(peak > 0, "{label}: the strike m1 never drew");
        assert_eq!(
            before, after,
            "{label}: a sky star's trace changed when the probe changed under it"
        );
    }
}

#[test]
fn the_population_cap_never_refuses_a_newborn() {
    let t0 = Instant::now();
    let mut sky = Stardust::new();
    for k in 0..400u32 {
        let at = t0 + Duration::from_millis(u64::from(k));
        let mut s = star(StarClass::M2, StarLane::Fan, at, TINT_WHITE_RGB);
        s.seed = k;
        sky.sow(s);
        let live = sky.live_iter().filter(|s| s.finish.is_none()).count();
        assert!(live <= STAR_CAP, "{live} live stars over the cap at {k}");
        assert!(
            sky.live() <= STAR_POOL_MAX,
            "the pool overran its allocation at {k}: {}",
            sky.live()
        );
        assert!(
            sky.live_iter().any(|p| p.seed == k),
            "birth {k} was refused at the cap"
        );
    }
}

#[test]
fn stars_over_the_cap_finish_oldest_first_within_forty_ms() {
    let c = cfg(true);
    let t0 = Instant::now();
    let mut sky = sky();
    for k in 0..STAR_CAP as u32 {
        let class = if k == 5 { StarClass::M3 } else { StarClass::M1 };
        let mut s = star(
            class,
            StarLane::Strike,
            t0 + Duration::from_millis(u64::from(k)),
            TINT_WHITE_RGB,
        );
        s.seed = k;
        // A row clear of the caret's cell, which keeps every star off.
        s.x = 100.0 + 5.0 * k as f32;
        s.y = 200.0;
        sky.sow(s);
    }
    let evict_at = t0 + Duration::from_millis(41);
    let mut newborn = star(StarClass::M1, StarLane::Strike, evict_at, TINT_WHITE_RGB);
    newborn.seed = 1000;
    newborn.x = 400.0;
    newborn.y = 200.0;
    sky.sow(newborn);
    let finishing: Vec<u32> = sky
        .live_iter()
        .filter(|s| s.finish.is_some())
        .map(|s| s.seed)
        .collect();
    assert_eq!(
        finishing,
        vec![0],
        "the star put on its finish is not the oldest"
    );

    let lit = |sky: &mut Stardust, ms: u64, x: i32| {
        let sc = frame_at(sky, evict_at + Duration::from_millis(ms), &c);
        lum(px(&sc.out, x, 200)) > 0
    };
    assert!(
        lit(&mut sky, 10, 100),
        "the evicted star popped instead of finishing"
    );
    assert!(
        !lit(&mut sky, 40, 100),
        "the evicted star outlived its 40 ms finish"
    );
    assert!(
        lit(&mut sky, 40, 125),
        "the young grain (#5) was evicted instead of the oldest"
    );
}

#[test]
fn an_earned_hero_is_never_demoted_by_spacing_but_a_dealt_one_is() {
    let c = cfg(true);
    let t0 = Instant::now();
    let ribbon = Ribbon::new();

    for class in [TypedClass::Capital, TypedClass::Bang] {
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        for col in 10..13u16 {
            sky.on_event(&typed(class), t0, &ctx(t0, &c, (5, col + 1)), &ribbon);
        }
        assert_eq!(
            sky.live_iter().filter(|p| p.class == StarClass::M1).count(),
            3,
            "adjacent {class:?} keys were demoted by the spacing law"
        );
    }

    let m1_draw = |row: u16, col: u16| {
        hash01(mix32(cell_hash(row, col, 0) ^ SALT_STRIKE)) < 1.0 / DEAL_M1_IN as f32
    };
    let row = (1..40u16)
        .find(|&row| {
            let draws: Vec<u16> = (0..20u16).filter(|&col| m1_draw(row, col)).collect();
            draws.len() >= 3
                && draws
                    .windows(2)
                    .any(|w| w[1] - w[0] >= HERO_MIN_SPACING_CELLS)
        })
        .expect("some row deals three heroes in twenty cells");
    let mut sky = Stardust::new();
    sky.probe_mut().probe_row(i32::from(row) - 1, &[false; 120]);
    for col in 0..20u16 {
        sky.on_event(
            &typed(TypedClass::Glyph),
            t0,
            &ctx(t0, &c, (row, col + 1)),
            &ribbon,
        );
    }
    assert!(sky.live() < STAR_CAP, "the fixture ran into the pool cap");
    let heroes: Vec<i32> = sky
        .live_iter()
        .filter(|p| p.class == StarClass::M1)
        .map(col_of)
        .collect();
    assert_eq!(
        heroes.len(),
        usize::from(HERO_MAX_LIVE_PER_ROW),
        "three or more dealt heroes on one row left {} live",
        heroes.len()
    );
    for (i, a) in heroes.iter().enumerate() {
        for b in &heroes[i + 1..] {
            assert!(
                (a - b).abs() >= i32::from(HERO_MIN_SPACING_CELLS),
                "dealt heroes at columns {a} and {b} are closer than 3 cells"
            );
        }
    }

    let later = t0 + Duration::from_secs(2);
    let mut sc = Scratch::default();
    sky.emit(&ctx(later, &c, (row, 1)), &mut sc.frame());
    assert!(sky.at_rest());
    sky.on_event(
        &typed(TypedClass::Capital),
        later,
        &ctx(later, &c, (row, 1)),
        &ribbon,
    );
    assert_eq!(
        sky.live_iter().filter(|p| p.class == StarClass::M1).count(),
        1
    );
}

#[test]
fn no_two_sky_stars_share_a_pixel() {
    let c = cfg(true);
    let t0 = Instant::now();
    let ribbon = Ribbon::new();
    let mut sky = Stardust::new();
    sky.probe_mut().probe_row(4, &[false; 120]);
    let mut cx = ctx(t0, &c, (5, 1));
    cx.disp = 0.6;
    cx.birth_disp = 0.6;
    for col in 0..119u16 {
        cx.caret = (5, col + 1);
        sky.on_event(&typed(TypedClass::Glyph), t0, &cx, &ribbon);
    }
    let mut strike_per_cell = std::collections::BTreeMap::new();
    let mut pixels = std::collections::BTreeSet::new();
    for s in sky.live_iter() {
        assert!(
            pixels.insert((s.x as i32, s.y as i32)),
            "two sky stars share pixel ({}, {})",
            s.x,
            s.y
        );
        if s.lane == StarLane::Strike && s.v0 != (0.0, 0.0) {
            *strike_per_cell.entry(col_of(s)).or_insert(0) += 1;
        }
    }
    assert!(
        strike_per_cell.values().all(|&n| n == 1),
        "a cell carries two m1/m2 strike stars: {strike_per_cell:?}"
    );
    assert!(
        sky.live() >= STAR_CAP,
        "the dense sky holds only {} stars",
        sky.live()
    );
}

#[test]
fn the_second_grain_is_priced_on_the_honest_spine() {
    let c = cfg(true);
    let t0 = Instant::now();
    let ribbon = Ribbon::new();
    let lead_grains = |disp: f32, birth: f32| {
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let mut cx = ctx(t0, &c, (5, 10));
        cx.disp = disp;
        cx.birth_disp = birth;
        sky.on_event(&typed(TypedClass::Space), t0, &cx, &ribbon);
        assert_eq!(sky.live(), 0, "a space dealt a star");
        sky.on_event(&typed(TypedClass::Glyph), t0, &cx, &ribbon);
        sky.live_iter().filter(|s| col_of(s) == 10).count()
    };
    assert_eq!(lead_grains(0.1, 0.9), 0, "a resume bought the dense sky");
    assert_eq!(
        lead_grains(0.6, 0.6),
        1,
        "a hot spine did not throw its second grain"
    );
}

#[test]
fn hero_births_and_glints_share_one_budget() {
    let c = cfg(true);
    let t0 = Instant::now();
    let ribbon = Ribbon::new();
    let mut sky = Stardust::new();
    sky.probe_mut().probe_row(4, &[false; 120]);
    sky.budget.refill(t0);

    let mut glints = 0usize;
    for k in 0..12u16 {
        sky.on_event(
            &typed(TypedClass::Capital),
            t0,
            &ctx(t0, &c, (5, 10 + k * 4)),
            &ribbon,
        );
        glints += sky.take_glints().count();
    }
    let heroes = sky.live_iter().filter(|p| p.class == StarClass::M1).count();
    assert_eq!(heroes, 12, "an earned hero was demoted for want of a token");
    assert_eq!(
        glints, GLINT_CAP as usize,
        "{glints} glints from a bucket of {GLINT_CAP}"
    );

    let mut sky = Stardust::new();
    sky.probe_mut().probe_row(4, &[false; 120]);
    let before = sky.budget.tokens();
    for k in 0..20u16 {
        sky.on_event(
            &typed(TypedClass::Glyph),
            t0,
            &ctx(t0, &c, (5, 10 + k)),
            &ribbon,
        );
    }
    assert!(sky.live_iter().any(|p| p.class == StarClass::M3));
    let spent = before - sky.budget.tokens();
    let dealt_heroes = sky.live_iter().filter(|p| p.class == StarClass::M1).count() as f32;
    assert!(
        spent <= dealt_heroes,
        "a grain or a medium star spent {spent} tokens"
    );

    let mut sky = Stardust::new();
    sky.probe_mut().probe_row(4, &[false; 120]);
    sky.budget.empty();
    for col in 0..120u16 {
        sky.on_event(
            &typed(TypedClass::Glyph),
            t0,
            &ctx(t0, &c, (5, col + 1)),
            &ribbon,
        );
    }
    assert_eq!(
        sky.live_iter().filter(|p| p.class == StarClass::M1).count(),
        0,
        "a dealt hero was born on a dry bucket"
    );

    let mut sky = Stardust::new();
    sky.probe_mut().probe_row(4, &[false; 120]);
    sky.budget.empty();
    sky.on_event(
        &typed(TypedClass::Capital),
        t0,
        &ctx(t0, &c, (5, 30)),
        &ribbon,
    );
    assert_eq!(
        sky.take_glints().count(),
        0,
        "a hero chimed on top of the rain"
    );
    assert!(
        sky.live_iter().any(|p| p.class == StarClass::M1),
        "an EARNED hero was demoted for want of a token"
    );
}

#[test]
fn a_paid_hero_cues_one_glint_at_its_own_column() {
    let c = cfg(true);
    let t0 = Instant::now();
    let ribbon = Ribbon::new();
    let mut sky = Stardust::new();
    sky.probe_mut().probe_row(4, &[false; 120]);
    sky.on_event(
        &typed(TypedClass::Capital),
        t0,
        &ctx(t0, &c, (5, 21)),
        &ribbon,
    );
    let sc = frame_at(&mut sky, t0, &c);
    assert_eq!(
        sc.cues.len(),
        1,
        "one paid hero did not cue exactly one glint"
    );
    assert_eq!(sc.cues[0].col, 20, "the glint is not at the hero's column");
    match sc.cues[0].kind {
        SoundKind::Stardust { twinkle_hz } => assert!(
            (SCINT_F_MIN as u8..=SCINT_F_MAX as u8).contains(&twinkle_hz),
            "twinkle_hz {twinkle_hz} is outside the seeded 8-12 Hz band"
        ),
        other => panic!("a hero cued {other:?}"),
    }
}

#[test]
fn a_row_zero_hero_clipped_off_the_glass_spends_nothing() {
    let c = cfg(true);
    let t0 = Instant::now();
    let ribbon = Ribbon::new();
    for (head, want) in [(0u16, 0usize), (40, 1)] {
        let mut sky = Stardust::new();
        let before = sky.budget.tokens();
        let mut cx = ctx(t0, &c, (0, 5));
        cx.geom.head = head;
        cx.geom.origin_y = head;
        sky.on_event(&typed(TypedClass::Capital), t0, &cx, &ribbon);
        let heroes = sky.live_iter().filter(|s| s.class == StarClass::M1).count();
        assert_eq!(heroes, want, "head {head}: {heroes} heroes");
        assert_eq!(sky.take_glints().count(), want, "head {head}: glint count");
        assert_eq!(
            (before - sky.budget.tokens()) as usize,
            want,
            "head {head}: a clipped hero spent a token"
        );
    }
}

#[test]
fn the_frame_that_echoes_the_key_already_shows_its_field_star() {
    let c = cfg(true);
    let t0 = Instant::now();
    let mut sky = sky();
    let mut field = star(StarClass::M3, StarLane::Field, t0, TINT_WHITE_RGB);
    field.x = 150.0;
    sky.sow(field);
    let echo = t0 + Duration::from_micros(300);
    let a = field.envelope(echo, false);
    assert!(
        a >= BIRTH_EDGE_FLOOR,
        "a field star's birth envelope is {a} on the echo frame"
    );
    let sc = frame_at(&mut sky, echo, &c);
    assert!(
        lum(px(&sc.out, 150, 100)) > 0,
        "the echo frame shows no field star"
    );
    let full = t0 + Duration::from_secs_f32(super::super::timing::EDGE_IN_S);
    assert!(
        (field.envelope(full, false) - 1.0).abs() < 1e-4,
        "the edge-in does not reach full: {}",
        field.envelope(full, false)
    );
}

#[test]
fn a_field_star_rides_its_cell_s_envelope_not_the_grain_s_life() {
    let c = cfg(true);
    let t0 = Instant::now();
    let ribbon = Ribbon::new();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let fixture = || {
        let mut sky = sky();
        let mut field = star(StarClass::M3, StarLane::Field, t0, TINT_WHITE_RGB);
        field.x = 150.0;
        sky.sow(field);
        sky.sow(star(StarClass::M3, StarLane::Strike, t0, TINT_WHITE_RGB));
        sky
    };
    let field_alpha = |sky: &Stardust, ms: u64| {
        sky.live_iter()
            .find(|s| s.lane == StarLane::Field)
            .map_or(0.0, |s| s.envelope(at(ms), false))
    };

    let mut sky = fixture();
    let mut next_key = 83u64;
    let mut type_until = |sky: &mut Stardust, ms: u64| {
        while next_key <= ms {
            let cx = ctx(at(next_key), &c, (3, 10));
            sky.on_event(&typed(TypedClass::Space), at(next_key), &cx, &ribbon);
            next_key += 83;
        }
    };
    let grain_dead = (StarClass::M3.life_s(StarLane::Strike) * 1000.0).ceil() as u64 + 1;
    let melt_from = ((1.0 - EXPIRY_MELT_SHARE) * FIELD_LIFE_S * 1000.0).round() as u64;
    for ms in [grain_dead, melt_from.saturating_sub(50)] {
        type_until(&mut sky, ms);
        let a = field_alpha(&sky, ms);
        assert!(
            (a - 1.0).abs() < 1e-3,
            "a field grain under a typing hand is at alpha {a} at {ms} ms, not full"
        );
    }
    let sc = frame_at(&mut sky, at(melt_from.saturating_sub(50)), &c);
    assert!(
        lum(px(&sc.out, 150, 100)) > 0,
        "the field grain died with the grain's life"
    );
    assert_eq!(
        lum(px(&sc.out, 200, 100)),
        0,
        "a strike grain outlived StarClass::M3.life_s(StarLane::Strike)"
    );
    type_until(&mut sky, melt_from + 20);
    let a_early = field_alpha(&sky, melt_from + 20);
    type_until(&mut sky, melt_from + 220);
    let a_late = field_alpha(&sky, melt_from + 220);
    assert!(
        a_early < 1.0 && a_late < a_early && a_late > STAR_CULL_ALPHA,
        "the field grain does not melt on the cell's curve: {a_early} → {a_late}"
    );
    let sc = frame_at(&mut sky, at(melt_from + 220), &c);
    assert!(lum(px(&sc.out, 150, 100)) > 0, "gone before its melt");
    let horizon = (FIELD_LIFE_S * 1000.0).round() as u64;
    type_until(&mut sky, horizon);
    let sc = frame_at(&mut sky, at(horizon + 1), &c);
    assert!(
        sc.out.is_empty() && sky.live() == 0,
        "a field star outlived the exit swoosh"
    );

    let mut sky = fixture();
    let hold = (FIELD_HOLD_MS as u64).saturating_sub(1);
    let a = field_alpha(&sky, hold);
    assert!(
        (a - 1.0).abs() < 1e-3,
        "a field grain is at alpha {a} inside its hold"
    );
    let (a500, a700) = (field_alpha(&sky, 500), field_alpha(&sky, 700));
    assert!(
        a500 < 1.0 && a700 <= 0.5 && a700 > STAR_CULL_ALPHA,
        "the field fade is not a half-life from the last key: {a500} → {a700}"
    );
    let cull = (field_fade_life_s() * 1000.0).round() as u64;
    let sc = frame_at(&mut sky, at(cull - 10), &c);
    assert!(
        lum(px(&sc.out, 150, 100)) > 0,
        "the field grain went out before its fade's cull"
    );
    let sc = frame_at(&mut sky, at(cull + 1), &c);
    assert!(
        sc.out.is_empty() && sky.at_rest(),
        "a field star outlived the field fade after the hand stopped"
    );
}

#[test]
fn a_field_star_is_pinned_and_finishes_with_its_cell() {
    let c = cfg(true);
    let t0 = Instant::now();
    let ribbon = Ribbon::new();
    let col = (0..120u16)
        .find(|&col| {
            let h = cell_hash(5, col, 0);
            deal(h, SALT_FIELD, FIELD_DEAL_IN, 1.0) && deal(h, SALT_FIELD_M2, FIELD_M2_IN, 1.0)
        })
        .expect("some column deals a field m2");
    let mut sky = Stardust::new();
    sky.probe_mut().probe_row(4, &[false; 120]);
    sky.on_event(
        &typed(TypedClass::Glyph),
        t0,
        &ctx(t0, &c, (5, col + 1)),
        &ribbon,
    );
    let field: Vec<&Star> = sky
        .live_iter()
        .filter(|s| s.lane == StarLane::Field)
        .collect();
    assert_eq!(
        field.len(),
        1,
        "the dealt cell carries {} field stars",
        field.len()
    );
    assert_eq!(field[0].class, StarClass::M2);
    assert_eq!(field[0].v0, (0.0, 0.0), "a field star lifted");

    let erase_at = t0 + Duration::from_millis(50);
    sky.on_event(
        &Event::Erase,
        erase_at,
        &ctx(erase_at, &c, (5, col)),
        &ribbon,
    );
    assert!(
        sky.live_iter()
            .any(|s| s.lane == StarLane::Field && s.finish.is_some()),
        "the retracted cell's field star was not put on the retract"
    );
    let later = t0 + Duration::from_millis(200);
    let mut sc = Scratch::default();
    sky.emit(&ctx(later, &c, (5, col)), &mut sc.frame());
    assert!(
        !sky.live_iter().any(|s| s.lane == StarLane::Field),
        "the field star outlived its cell's retract"
    );
}

#[test]
fn focus_loss_embers_the_sky_instead_of_popping_it() {
    let c = cfg(true);
    let t0 = Instant::now();
    let ribbon = Ribbon::new();
    let mut sky = sky();
    for k in 0..3u32 {
        let mut s = star(StarClass::M1, StarLane::Strike, t0, TINT_WHITE_RGB);
        s.seed = k;
        s.x = 100.0 + 20.0 * k as f32;
        sky.sow(s);
    }
    let blur = t0 + Duration::from_millis(10);
    sky.on_event(&Event::Focus(false), blur, &ctx(blur, &c, (3, 10)), &ribbon);
    let peak = |sky: &mut Stardust, ms: u64| {
        let sc = frame_at(sky, blur + Duration::from_millis(ms), &c);
        (0..3)
            .map(|k| lum(px(&sc.out, 100 + 20 * k, 100)))
            .max()
            .unwrap_or(0)
    };
    let at_1 = peak(&mut sky, 1);
    let at_100 = peak(&mut sky, 100);
    assert!(at_1 > 0, "the sky popped on the blur frame");
    assert!(
        at_100 > 0 && at_100 < at_1,
        "the ember is not a fade: {at_1} → {at_100}"
    );
    assert_eq!(peak(&mut sky, 300), 0, "a star outlived the 300 ms ember");
    assert!(sky.at_rest());
}

#[test]
fn an_idle_sky_draws_exactly_nothing() {
    let c = cfg(true);
    let t0 = Instant::now();
    let mut sky = sky();
    let sc = frame_at(&mut sky, t0, &c);
    assert!(sc.out.is_empty() && sc.halos.is_empty() && sc.under.is_empty());
    assert!(sky.at_rest());
    assert!(sky.next_change_deadline(t0).is_none());

    sky.sow(star(StarClass::M1, StarLane::Strike, t0, TINT_WHITE_RGB));
    assert!(!sky.at_rest() && sky.next_change_deadline(t0).is_some());
    let life = Duration::from_secs_f32(StarClass::M1.life_s(StarLane::Strike));
    let sc = frame_at(&mut sky, t0 + life + Duration::from_millis(1), &c);
    assert!(sc.out.is_empty() && sc.halos.is_empty());
    assert!(sky.at_rest() && sky.next_change_deadline(t0).is_none());
}

#[test]
fn every_v2_mark_forks_to_source_over_on_light() {
    let light = cfg(false);
    let t0 = Instant::now();
    for (class, lane, _) in every_class() {
        let mut sky = sky();
        sky.sow(star(class, lane, t0, spectrum_snap(0.3)));
        let sc = frame_at(&mut sky, t0 + Duration::from_millis(1), &light);
        assert!(
            sc.out.is_empty(),
            "{class:?}/{lane:?} left an additive quad on light"
        );
        assert!(
            !sc.halos.is_empty(),
            "{class:?}/{lane:?} drew nothing on light"
        );
        for h in &sc.halos {
            assert_eq!(
                h.mode,
                HaloMode::Over,
                "{class:?}/{lane:?} emitted an additive halo"
            );
            let a = h.color >> 24;
            assert!(
                a >= 1 && a as f32 <= LIGHT_ALPHA_CAP,
                "{class:?}/{lane:?} alpha {a}"
            );
            if h.rx != h.ry {
                assert!(
                    a as f32 <= InkRole::OverText.alpha_cap(),
                    "{class:?}/{lane:?} needle alpha {a} over the over-text cap"
                );
            }
        }
        let round = sc.halos.iter().filter(|h| h.rx == h.ry).count();
        if class == StarClass::M3 {
            let shapes: std::collections::BTreeSet<_> =
                sc.halos.iter().map(|h| (h.rx, h.ry)).collect();
            let dots = shapes.iter().filter(|(rx, ry)| rx == ry).count();
            let needles: Vec<_> = shapes.iter().filter(|(rx, ry)| rx != ry).collect();
            assert_eq!(dots, 1, "a light grain is not one round dot: {shapes:?}");
            assert!(
                needles.len() == 2
                    && needles.iter().all(|(rx, ry)| (*rx).min(*ry) == 1)
                    && needles[0] == &(needles[1].1, needles[1].0),
                "a light grain's tips are not two crossed 1-px needles: {shapes:?}"
            );
            let centres: std::collections::BTreeSet<_> =
                sc.halos.iter().map(|h| (h.cx, h.cy)).collect();
            assert_eq!(centres.len(), 1);
        } else {
            assert!(sc.halos.len() > round, "a light {class:?} has no arms");
        }
    }
    for (class, glints) in [(StarClass::M1, 4), (StarClass::M2, 0)] {
        let mut sky = sky();
        sky.sow(star(class, StarLane::Strike, t0, TINT_GOLD_RGB));
        let sc = frame_at(&mut sky, t0 + Duration::from_millis(1), &light);
        let off_centre = sc
            .halos
            .iter()
            .filter(|h| (h.rx, h.ry) == (1, 1) && (h.cx, h.cy) != (200, 100))
            .count();
        assert_eq!(
            off_centre, glints,
            "a gold {class:?} on light threw {off_centre} glint dots, not {glints}"
        );
    }
}

const PAPER: u32 = 0x00FF_FFFF;

fn ink_on_paper(sc: &Scratch, x0: i32, y0: i32, w: usize, h: usize) -> u64 {
    composite_over(sc, PAPER, x0, y0, w, h)
        .iter()
        .map(|&p| u64::from(765 - ((p >> 16) & 0xff) - ((p >> 8) & 0xff) - (p & 0xff)))
        .sum()
}

#[test]
fn a_light_star_s_ink_is_steady_on_the_size_clock() {
    const STEP_MAX: f32 = 1.35;
    let light = cfg(false);
    let t0 = Instant::now();
    let mut failures = Vec::new();
    for g in [geom_cell(15, 28), geom()] {
        for class in [StarClass::M3, StarClass::M2, StarClass::M1] {
            let mut sky = sky();
            sky.sow(star(class, StarLane::Strike, t0, spectrum_snap(0.3)));
            let life_ms = (class.life_s(StarLane::Strike) * 1000.0) as u64;
            let mut bodies = std::collections::BTreeSet::new();
            let (mut prev, mut worst, mut worst_at) = (None::<(u64, bool)>, 1.0f32, 0u64);
            for ms in 0..life_ms {
                let now = t0 + Duration::from_millis(ms);
                let held = sky.stars.iter().any(|s| s.in_hold(now));
                let sc = frame_in(&mut sky, g, now, &light);
                let Some(first) = sc.halos.first() else {
                    prev = None;
                    continue;
                };
                bodies.insert((first.rx, first.ry));
                let ink = ink_on_paper(&sc, 170, 70, 60, 60);
                if let Some((p, was_held)) = prev
                    && was_held == held
                {
                    let step = p.max(ink) as f32 / p.min(ink).max(1) as f32;
                    if step > worst {
                        (worst, worst_at) = (step, ms);
                    }
                }
                prev = Some((ink, held));
            }
            eprintln!(
                "{class:?} at {}x{}: ink steps at most {worst:.2}x between 1 ms frames (at \
                 {worst_at} ms); body radii {bodies:?}",
                g.cw, g.ch
            );
            if bodies.len() != 1 {
                failures.push(format!(
                    "{class:?} at ch {}: the dot or nucleus changes size, {bodies:?}",
                    g.ch
                ));
            }
            if worst > STEP_MAX {
                failures.push(format!(
                    "{class:?} at ch {}: the ink steps {worst:.2}x in 1 ms at {worst_at} ms, \
                     over the dark grain's {STEP_MAX}x",
                    g.ch
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "a light star's ink rides the size clock:\n{}",
        failures.join("\n")
    );
}

#[test]
fn a_fan_star_is_born_at_full_and_clears_its_cell_inside_its_hold() {
    let c = cfg(true);
    let t0 = Instant::now();
    let g = geom();
    let cx = ctx(t0, &c, (5, 40));
    let (ox, oy) = g.cell_center(5, 40);
    let origin = (ox.round() as i32, oy.round() as i32);
    let reach = 2.55 * g.ch as f32;
    let half = (g.cw as i32 / 2, g.ch as i32 / 2);
    let at = t0 + Duration::from_millis(HOLD_M2_TRANSIENT_MS as u64);
    for seed in 0..12u32 {
        let mut sky = Stardust::new();
        sky.sow_fan(
            t0,
            FanSow {
                at_px: (ox, oy),
                reach,
                walk: LandingWalk::free(0.3),
                seed: seed.wrapping_mul(0x9E37_79B9),
                n: 9,
                hero: true,
            },
            &cx,
        );
        for s in sky.live_iter().filter(|s| s.class != StarClass::M3) {
            assert!(
                s.in_hold(at - Duration::from_millis(1)),
                "the fixture's own hold"
            );
            let (x, y) = s.pos(at, false);
            let (dx, dy) = ((x - origin.0).abs(), (y - origin.1).abs());
            assert!(
                dx > half.0 || dy > half.1,
                "seed {seed}: a {:?} fan star is still on the caret cell ({dx}, {dy} px \
                 from its centre) at the end of its hold",
                s.class
            );
        }
    }
}

#[test]
fn halos_are_budgeted_per_star_not_per_split() {
    let c = cfg(true);
    let t0 = Instant::now();
    let mut sky = sky();
    // The row clear of the caret's neighbourhood at (3, 10), which keeps
    // its row beside it dark ([`crate::rainbow_kitty::DARK_CARET_ROW_CELLS`]).
    for k in 0..17u32 {
        let mut s = star(StarClass::M2, StarLane::Strike, t0, spectrum_snap(0.1));
        s.seed = peak_seed();
        s.x = 150.0 + 20.0 * k as f32;
        s.y = 93.0;
        sky.sow(s);
    }
    let sc = frame_at(&mut sky, t0, &c);
    let haloed: std::collections::BTreeSet<u16> = sc.halos.iter().map(|h| h.cx).collect();
    assert_eq!(
        haloed.len(),
        STARDUST_HALO_BUDGET,
        "{} stars carry a halo",
        haloed.len()
    );
    assert!(
        sc.halos.len() > STARDUST_HALO_BUDGET,
        "the halos did not split across the row boundary"
    );
}

/// On a dark page no star stands on the caret's block: after the
/// gallery's dark Return a fan star's crosshair stood at the new caret's
/// corner, its hairline at the block's edge (looks7 M3, `e120`). A star
/// whose arm at its peak reaches the caret's cell or within
/// [`crate::rainbow_kitty::DARK_CARET_GAP_PX`] of it is not drawn, over its
/// life; with the caret a few cells off (the negative control) the same
/// stars are.
#[test]
fn no_dark_star_stands_on_the_caret_s_block() {
    let c = cfg(true);
    let g = geom();
    let t0 = Instant::now();
    // The caret's cell at (3, 10): x 90..99, y 94..112.
    let drawn = |caret: (u16, u16)| -> usize {
        let mut n = 0;
        for (x, y) in [(94.0, 103.0), (104.0, 110.0), (86.0, 90.0), (95.0, 116.0)] {
            let mut sky = sky();
            let mut s = star(StarClass::M3, StarLane::Fan, t0, TINT_WHITE_RGB);
            (s.x, s.y) = (x, y);
            sky.sow(s);
            for at in [0u64, 30, 90, 160] {
                let mut sc = Scratch::default();
                let mut f = sc.frame();
                sky.emit(
                    &ctx_in(g, t0 + Duration::from_millis(at), &c, caret),
                    &mut f,
                );
                n += sc.out.len() + sc.halos.len();
            }
        }
        n
    };
    let on = drawn((3, 10));
    assert_eq!(on, 0, "{on} star lights stand on the dark caret's block");
    assert!(
        drawn((3, 30)) > 0,
        "control: off the caret the stars are not drawn either"
    );
}

/// looks9 Q10: after Solarized Dark's Return at the bottom a faint navy
/// cross stood two cells past the new caret on its row
/// (`rainbow-kitty-pet-dark-enter-bottom.e120`), where the next keys
/// land. On a dark page a star on the caret's row within
/// [`crate::rainbow_kitty::DARK_CARET_ROW_CELLS`] of its block is not
/// drawn, either side; with the caret three rows off (the negative
/// control) the same stars are.
#[test]
fn no_dark_star_stands_on_the_caret_s_row_beside_it() {
    let c = cfg(true);
    let g = geom();
    let t0 = Instant::now();
    // The caret's cell at (3, 10): x 90..99, y 94..112.
    let drawn = |caret: (u16, u16)| -> usize {
        let mut n = 0;
        for (x, y) in [(110.0, 103.0), (118.0, 106.0), (74.0, 100.0), (66.0, 108.0)] {
            let mut sky = sky();
            let mut s = star(StarClass::M3, StarLane::Fan, t0, TINT_WHITE_RGB);
            (s.x, s.y) = (x, y);
            sky.sow(s);
            for at in [0u64, 30, 90, 160] {
                let mut sc = Scratch::default();
                let mut f = sc.frame();
                sky.emit(
                    &ctx_in(g, t0 + Duration::from_millis(at), &c, caret),
                    &mut f,
                );
                n += sc.out.len() + sc.halos.len();
            }
        }
        n
    };
    let on = drawn((3, 10));
    assert_eq!(
        on, 0,
        "{on} star lights stand on the dark caret's row beside it"
    );
    assert!(
        drawn((6, 10)) > 0,
        "control: off the caret's row the stars are not drawn either"
    );
}

/// looks9 round 3's review: Q10's cull of a dark star on the caret's row
/// beside its block is read once, at the star's first paint, from the
/// caret then. Read every frame, a lit star the caret typed past went
/// dark and came back, and one born beside a resting caret popped in
/// mid-life once typing moved the caret on. A lit star the caret comes
/// within two cells of on its row, and leaves, is drawn every frame; a
/// star first painted two cells behind a resting caret is never drawn,
/// however far typing then takes the caret. Negative control: read every
/// frame (`beside_caret_each_frame`), the first goes dark and comes back
/// and the second pops in.
#[test]
fn a_dark_star_beside_the_caret_is_judged_once() {
    let c = cfg(true);
    let g = geom();
    let t0 = Instant::now();
    // One frame each 25 ms, the caret at each of `carets` in turn: `#`
    // where the star is drawn.
    let run = |each_frame: bool, x: f32, carets: &[u16]| -> String {
        let mut sky = sky();
        sky.beside_caret_each_frame = each_frame;
        let mut s = star(StarClass::M1, StarLane::Strike, t0, TINT_WHITE_RGB);
        // On the caret's row 3 (y 94..112).
        (s.x, s.y) = (x, 103.0);
        sky.sow(s);
        let mut seen = String::new();
        for (k, &col) in carets.iter().enumerate() {
            let mut sc = Scratch::default();
            let mut f = sc.frame();
            let at = t0 + Duration::from_millis(25 * k as u64);
            sky.emit(&ctx_in(g, at, &c, (3, col)), &mut f);
            seen.push(if sc.out.len() + sc.halos.len() > 0 {
                '#'
            } else {
                '.'
            });
        }
        seen
    };
    // Lit with the caret four cells off (cells 10..12 of x 90..117),
    // then the caret two cells off, then gone on.
    let passing = [10, 10, 10, 10, 11, 11, 12, 12, 12, 12, 30, 30, 30, 30];
    let lit = run(false, 132.0, &passing);
    assert!(
        !lit.contains('.'),
        "a lit star the caret comes beside goes dark: {lit}"
    );
    let old = run(true, 132.0, &passing);
    assert!(
        old.starts_with('#') && old.contains('.') && old.ends_with('#'),
        "control: read every frame, the lit star does not go dark and come back: {old}"
    );
    // Two cells behind a resting caret at cell 10, then typing on.
    let typing = [10, 10, 10, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20];
    let born = run(false, 76.0, &typing);
    assert!(
        !born.contains('#'),
        "a star first painted beside the caret pops in: {born}"
    );
    let old = run(true, 76.0, &typing);
    assert!(
        old.starts_with('.') && old.ends_with('#'),
        "control: read every frame, the star does not pop in: {old}"
    );
}

#[test]
fn an_over_budget_frame_keeps_every_star_s_core() {
    let c = cfg(true);
    let t0 = Instant::now();
    let mut sky = sky();
    for k in 0..STAR_CAP as u32 {
        let mut s = star(StarClass::M1, StarLane::Strike, t0, TINT_WHITE_RGB);
        s.seed = peak_seed();
        // A row clear of the caret's cell, which keeps every star off.
        s.x = 20.0 + 24.0 * k as f32;
        s.y = 200.0;
        sky.sow(s);
    }
    for k in 0..STAR_FINISH_SLACK as u32 {
        let mut s = star(StarClass::M3, StarLane::Strike, t0, TINT_WHITE_RGB);
        s.seed = peak_seed();
        s.x = 20.0 + 24.0 * k as f32;
        s.y = 160.0;
        sky.sow(s);
    }
    assert_eq!(sky.live(), STAR_POOL_MAX, "the fixture is not a full pool");
    let sc = frame_at(&mut sky, t0, &c);
    assert!(
        sc.out.len() <= STARDUST_QUAD_BUDGET,
        "{} quads over the budget",
        sc.out.len()
    );
    for k in 0..STAR_CAP as i32 {
        assert!(
            lum(px(&sc.out, 20 + 24 * k, 200)) > 0,
            "hero {k} lost its core to the budget"
        );
    }
    for k in 0..STAR_FINISH_SLACK as i32 {
        assert!(
            lum(px(&sc.out, 20 + 24 * k, 160)) > 0,
            "grain {k} lost its core to the budget"
        );
    }
}

/// On a dark page nothing coloured is drawn below α 0.12 (§3.2): near
/// the cull, at a twinkle trough, a hero's halo goes while its white core
/// (floor 0.20) still draws (replaces the pruned
/// `a_halo_is_culled_below_the_chroma_floor`).
#[test]
fn a_dark_halo_is_culled_below_the_chroma_floor_while_its_core_draws() {
    let c = cfg(true);
    let t0 = Instant::now();
    let mut sky = sky();
    let mut s = star(StarClass::M1, StarLane::Strike, t0, spectrum_snap(0.2));
    // A seed whose twinkle trough meets the end of life, so the band
    // `α ∈ [0.20, 0.218)` is swept.
    let life = StarClass::M1.life_s(StarLane::Strike);
    s.seed = (0u32..)
        .find(|&k| {
            s.seed = k;
            s.twinkle(t0 + Duration::from_secs_f32(life - 0.004)) < 0.58
        })
        .expect("some seed puts the trough at the life's end");
    sky.sow(s);
    let (mut haloed, mut culled_while_core_drawn) = (false, false);
    for ms in 0..(life * 1000.0) as u64 {
        let now = t0 + Duration::from_millis(ms);
        let sc = frame_at(&mut sky, now, &c);
        let s = sky.live_iter().next().expect("the hero is live");
        let a = s.envelope(now, false) * s.twinkle(now);
        haloed |= !sc.halos.is_empty();
        if a < CHROMA_CULL_ALPHA {
            assert!(
                sc.halos.is_empty(),
                "a halo at α·env {a} is under the chroma floor"
            );
            culled_while_core_drawn |= !sc.out.is_empty();
        }
    }
    assert!(haloed, "fixture: the hero never wore its halo");
    assert!(
        culled_while_core_drawn,
        "the sweep never reached the band where the halo culls before the core"
    );
}

#[test]
fn reduced_motion_freezes_both_clocks_and_takes_the_linear_fade() {
    let mut c = cfg(true);
    c.reduced_motion = true;
    let t0 = Instant::now();
    let mut sky = sky();
    let mut s = star(StarClass::M2, StarLane::Strike, t0, TINT_WHITE_RGB);
    s.v0 = (0.0, sky_lift_px_per_s(geom().ch as f32) / 1000.0);
    sky.sow(s);
    let life_ms = (StarClass::M2.life_s(StarLane::Strike) * 1000.0) as u64;
    let mut widths = std::collections::BTreeSet::new();
    for ms in (0..life_ms - 130).step_by(7) {
        let sc = frame_at(&mut sky, t0 + Duration::from_millis(ms), &c);
        widths.insert(sc.out.iter().map(|q| q.w).max().unwrap_or(0));
        assert!(
            lum(px(&sc.out, 200, 100)) > 0,
            "the mark drifted at {ms} ms"
        );
    }
    assert_eq!(
        widths.len(),
        1,
        "the arm clock ran under reduced motion: {widths:?}"
    );
}

#[test]
fn a_scroll_moves_the_sky_and_forgets_the_probe() {
    let t0 = Instant::now();
    let mut sky = sky();
    let mut high = star(StarClass::M2, StarLane::Strike, t0, TINT_WHITE_RGB);
    high.y = 50.0;
    sky.sow(high);
    sky.sow(star(StarClass::M2, StarLane::Strike, t0, TINT_WHITE_RGB));
    sky.translate_scroll(3, 18);
    assert_eq!(sky.live(), 1, "a star that left the grid was kept");
    assert_eq!(sky.live_iter().next().map(|s| s.y), Some(46.0));
    assert!(sky.probe().is_empty(), "the probe survived a scroll");
}

#[test]
fn a_band_move_moves_stars_in_the_band_and_clears_the_probe() {
    let t0 = Instant::now();
    let mut sky = sky();
    assert!(!sky.probe().is_empty(), "fixture: rows 2-4 are probed");
    let mut high = star(StarClass::M2, StarLane::Strike, t0, TINT_WHITE_RGB);
    high.y = 50.0;
    sky.sow(high);
    sky.sow(star(StarClass::M2, StarLane::Strike, t0, TINT_WHITE_RGB));
    for row in [5u16, 10] {
        sky.veil.push(Veil {
            row,
            col: 4,
            t: 0.3,
            gain: 1.0,
            born: t0,
            lit: t0,
            finish: None,
        });
    }

    sky.translate_band(4, 10, 1, 18, 0);

    assert_eq!(sky.live(), 2, "no star inside or outside the band was lost");
    let ys: Vec<f32> = sky.live_iter().map(|s| s.y).collect();
    assert!(
        ys.contains(&50.0),
        "the row-2 star is outside the band: {ys:?}"
    );
    assert!(ys.contains(&118.0), "the row-5 star rode to row 6: {ys:?}");
    assert_eq!(
        sky.veil.iter().map(|v| v.row).collect::<Vec<_>>(),
        vec![6],
        "the row-5 veil rode to row 6 and the row-10 veil left the band"
    );
    assert!(sky.probe().is_empty(), "the probe survived a band move");

    sky.translate_band(0, 3, -1, 18, 0);
    let ys: Vec<f32> = sky.live_iter().map(|s| s.y).collect();
    assert!(ys.contains(&32.0) && ys.contains(&118.0), "{ys:?}");
    sky.translate_band(0, 3, -2, 18, 0);
    assert_eq!(sky.live(), 1, "a star that left the band was kept");
    assert_eq!(sky.live_iter().next().map(|s| s.y), Some(118.0));

    let mut cold = Stardust::new();
    cold.sow(star(StarClass::M2, StarLane::Strike, t0, TINT_WHITE_RGB));
    cold.translate_band(4, 10, 1, 0, 0);
    assert_eq!(cold.live(), 0);
}

#[test]
fn band_moves_follow_the_earning_glyph_at_sky_and_footer_edges() {
    let now = Instant::now();
    let ribbon = Ribbon::new();
    let mut caught_pixel_only = 0;
    for tall in [false, true] {
        let mut config = cfg(true);
        config.ribbon_tall = tall;
        for top in [0u16, 4] {
            let bottom = top + 6;
            for home in [top, top + 1, bottom, bottom + 1] {
                for delta in [-1i16, 1] {
                    let mut sky = Stardust::new();
                    if let Some(above) = home.checked_sub(1) {
                        sky.probe_mut().probe_row(i32::from(above), &[false; 120]);
                    }
                    sky.probe_mut().probe_row(i32::from(home), &[false; 120]);
                    let context = ctx(now, &config, (home, 10));
                    assert!(sky.deal_star(
                        Deal {
                            class: StarClass::M2,
                            lane: StarLane::Field,
                            cell: (home, 10),
                            earned: false,
                            pull: None,
                            tint: None,
                        },
                        now,
                        &context,
                        &ribbon
                    ));
                    let before = *sky.live_iter().next().unwrap();
                    assert_eq!(before.home_row, Some(home));
                    let moved = i32::from(home) + i32::from(delta);
                    let expected = if home > bottom {
                        Some(home)
                    } else if moved < i32::from(top) || moved > i32::from(bottom) {
                        None
                    } else {
                        Some(moved as u16)
                    };
                    sky.translate_band(top, bottom, delta, geom().ch as u16, geom().origin_y);
                    if let Some(row) = expected {
                        let after = sky.live_iter().next().unwrap();
                        assert_eq!(after.home_row, Some(row));
                        let wanted_y =
                            before.y + (i32::from(row) - i32::from(home)) as f32 * geom().ch as f32;
                        assert_eq!(after.y, wanted_y);
                        assert_eq!(
                            (after.born, after.lit, after.seed),
                            (before.born, before.lit, before.seed)
                        );
                        let mut wrong_y = before.y;
                        let retained =
                            BandPx::new(top, bottom, delta, geom().ch as u16, geom().origin_y)
                                .point(&mut wrong_y);
                        if !retained || wrong_y != wanted_y {
                            caught_pixel_only += 1;
                        }
                    } else {
                        assert_eq!(sky.live(), 0, "a star outlived its ejected glyph row");
                    }
                }
            }
        }
    }
    assert!(
        caught_pixel_only >= 4,
        "the fixture must catch pixel-row ownership at both band edges"
    );
}

#[test]
fn sky_ownership_survives_uniform_scroll_before_a_band_move() {
    let now = Instant::now();
    let config = cfg(true);
    let mut sky = Stardust::new();
    sky.probe_mut().probe_row(6, &[false; 120]);
    sky.probe_mut().probe_row(7, &[false; 120]);
    assert!(sky.deal_star(
        Deal {
            class: StarClass::M2,
            lane: StarLane::Field,
            cell: (7, 10),
            earned: false,
            pull: None,
            tint: None,
        },
        now,
        &ctx(now, &config, (7, 10)),
        &Ribbon::new()
    ));
    let before = *sky.live_iter().next().unwrap();
    sky.translate_scroll(1, geom().ch as u16);
    assert_eq!(sky.live_iter().next().unwrap().home_row, Some(6));
    sky.translate_band(6, 10, 1, geom().ch as u16, geom().origin_y);
    let after = sky.live_iter().next().unwrap();
    assert_eq!((after.home_row, after.y), (Some(7), before.y));
    sky.translate_scroll(8, geom().ch as u16);
    assert_eq!(sky.live(), 0, "an ejected owner retires its sky star");
}

#[test]
fn the_size_clocks_next_step_is_solved_not_polled() {
    let t0 = Instant::now();
    let ch = geom().ch as f32;
    for class in [StarClass::M1, StarClass::M2, StarClass::M3] {
        for (f, seed) in [(8.0, 0x11u32), (10.3, 0x2222), (12.0, 0x3333_3333)] {
            let mut s = star(class, StarLane::Strike, t0, TINT_WHITE_RGB);
            s.f = f;
            s.seed = seed;
            let hold = class.envelope_s(StarLane::Strike).0;
            for k in 0..7 {
                let now = t0 + Duration::from_secs_f32(hold + 0.013 * k as f32);
                let dt = s
                    .next_arm_step_s(ch, now)
                    .expect("every class swings across an integer arm");
                assert!(
                    dt > 0.0 && dt <= 0.5 / f + 1e-4,
                    "{class:?} f={f}: step {dt} s outside (0, 1/(2f)]"
                );
                let here = arm_px(&s, ch, s.in_hold(now), now);
                let mut polled = None;
                let mut t = 0.0f32;
                while t < 0.2 {
                    t += 0.000_05;
                    let at = now + Duration::from_secs_f32(t);
                    if arm_px(&s, ch, s.in_hold(at), at) != here {
                        polled = Some(t);
                        break;
                    }
                }
                let polled = polled.expect("the polled arm never stepped");
                assert!(
                    (dt - polled).abs() < 1e-4,
                    "{class:?} f={f} k={k}: solved {dt} s, polled {polled} s"
                );
            }
        }
    }
}

#[test]
fn the_glint_toggle_is_solved_not_polled() {
    let t0 = Instant::now();
    let mut gold = star(StarClass::M1, StarLane::Strike, t0, TINT_GOLD_RGB);
    for (k, seed) in [0x5u32, 0x77, 0xABCD].into_iter().enumerate() {
        gold.seed = seed;
        let now = t0 + Duration::from_millis(30 * k as u64 + 1);
        let dt = gold.next_glint_toggle_s(now).expect("gold toggles");
        let here = gold.glinting(now);
        let mut t = 0.0f32;
        let polled = loop {
            t += 0.000_05;
            if gold.glinting(now + Duration::from_secs_f32(t)) != here {
                break t;
            }
            assert!(t < 1.0, "the glint never toggled");
        };
        assert!(
            (dt - polled).abs() < 1e-4,
            "seed {seed:#x}: solved {dt} s, polled {polled} s"
        );
    }
    let white = star(StarClass::M1, StarLane::Strike, t0, TINT_WHITE_RGB);
    assert!(white.next_glint_toggle_s(t0).is_none());
}

const SHIPPED_GROUND: u32 = 0x001A_1B26;

fn composite_over(sc: &Scratch, ground: u32, x0: i32, y0: i32, w: usize, h: usize) -> Vec<u32> {
    let mut canvas = vec![ground; w * h];
    for q in &sc.out {
        for y in i32::from(q.y)..i32::from(q.y) + i32::from(q.h) {
            for x in i32::from(q.x)..i32::from(q.x) + i32::from(q.w) {
                let (cx, cy) = (x - x0, y - y0);
                if cx < 0 || cy < 0 || cx as usize >= w || cy as usize >= h {
                    continue;
                }
                let p = &mut canvas[cy as usize * w + cx as usize];
                *p = aterm_render::over_premul(*p, q.color, q.alpha);
            }
        }
    }
    for mode in [HaloMode::Add, HaloMode::Over] {
        for a in sc.halos.iter().filter(|a| a.mode == mode) {
            let (rx2, ry2) = (
                i32::from(a.rx) * i32::from(a.rx),
                i32::from(a.ry) * i32::from(a.ry),
            );
            for y in i32::from(a.y)..i32::from(a.y) + i32::from(a.h) {
                let ny = halo_row_ny(y - i32::from(a.cy), ry2);
                if ny >= 256 {
                    continue;
                }
                for x in i32::from(a.x)..i32::from(a.x) + i32::from(a.w) {
                    let wt = halo_weight(x - i32::from(a.cx), ny, rx2).min(255) as u8;
                    let (cx, cy) = (x - x0, y - y0);
                    if wt == 0 || cx < 0 || cy < 0 || cx as usize >= w || cy as usize >= h {
                        continue;
                    }
                    let p = &mut canvas[cy as usize * w + cx as usize];
                    *p = match mode {
                        HaloMode::Add => add_sat(*p, premul_rgb(a.color, wt)),
                        HaloMode::Over => aterm_render::over_rgb(
                            *p,
                            a.color,
                            wt.min(aterm_render::halo_over_cap(a.color)),
                        ),
                    };
                }
            }
        }
    }
    canvas
}

fn veils(sc: &Scratch) -> Vec<RainHalo> {
    sc.halos.iter().filter(|h| h.rx != h.ry).copied().collect()
}

#[test]
fn no_star_outshines_the_caret_s_floor_share_on_the_shipped_ground() {
    let t0 = Instant::now();
    let ceil = STAR_LIGHT_CEIL * 255.0;
    let tints: Vec<u32> = (0..SPECTRUM_STOPS)
        .map(spectrum_stop)
        .chain([TINT_GOLD_RGB, TINT_WHITE_RGB])
        .collect();
    let mut worst = (0.0f32, String::new());
    let mut white_hero_at_full = 0.0f32;
    for intensity in [0.7f32, 1.0] {
        let mut c = cfg(true);
        c.theme_bg = SHIPPED_GROUND;
        c.intensity = intensity;
        for (class, lane, _) in every_class() {
            for &tint in &tints {
                let mut sky = sky();
                sky.sow(star(class, lane, t0, tint));
                let life_ms = (class.life_s(lane) * 1000.0) as u64;
                let mut peak = 0.0f32;
                for ms in (0..life_ms).step_by(5) {
                    let sc = frame_at(&mut sky, t0 + Duration::from_millis(ms), &c);
                    for &p in &composite_over(&sc, SHIPPED_GROUND, 170, 70, 60, 60) {
                        peak = peak.max(relative_luminance(p) * 255.0);
                    }
                }
                if peak > worst.0 {
                    worst = (
                        peak,
                        format!("{class:?}/{lane:?} {tint:#08x} at intensity {intensity}"),
                    );
                }
                if class == StarClass::M1
                    && lane == StarLane::Strike
                    && tint == TINT_WHITE_RGB
                    && intensity == 1.0
                {
                    white_hero_at_full = peak;
                }
            }
        }
    }
    assert!(
        worst.0 <= ceil,
        "{} reached Y {:.1} over the caret law's {ceil:.1}",
        worst.1,
        worst.0
    );
    assert!(
        white_hero_at_full >= ceil - 1.0,
        "NON-VACUOUS: the white hero at full intensity stands at Y {white_hero_at_full:.1}, \
         not against the ceiling {ceil:.1}"
    );
}

#[test]
fn the_closer_dims_a_star_by_one_factor_until_its_arms_fit_and_leaves_one_that_fits() {
    let g = geom();
    let ceil = STAR_LIGHT_CEIL * 255.0;
    let y = |p: u32| relative_luminance(p) * 255.0;
    let yellow = spectrum_stop(2);
    let mut sc = Scratch::default();
    push_fx_rect(&mut sc.out, g, 300, 100, 1, 1, 0x0030_3030);
    let q0 = sc.out.len();
    push_fx_rect(
        &mut sc.out,
        g,
        200,
        100,
        1,
        1,
        premul_rgb(TINT_WHITE_RGB, 40),
    );
    push_fx_rect(&mut sc.out, g, 203, 100, 2, 1, premul_rgb(yellow, 61));
    push_fx_rect(
        &mut sc.out,
        g,
        203,
        100,
        1,
        1,
        premul_rgb(TINT_WHITE_RGB, 35),
    );
    let h0 = sc.halos.len();
    push_halo_quads(
        &mut sc.halos,
        g,
        usize::MAX,
        (200, 100),
        (15, 15),
        premul_rgb(yellow, 61),
        HaloMode::Add,
    );
    assert!(sc.halos.len() > h0, "no halo was laid");
    let before: Vec<u32> = sc.out[q0..].iter().map(|q| q.color).collect();
    let halo_before = sc.halos[h0].color;
    let watch: [((i32, i32), u32); SETTLE_PIXELS] = [
        ((200, 100), SHIPPED_GROUND),
        ((203, 100), SHIPPED_GROUND),
        ((197, 100), SHIPPED_GROUND),
        ((200, 103), SHIPPED_GROUND),
        ((200, 97), SHIPPED_GROUND),
        ((201, 100), SHIPPED_GROUND),
        ((199, 100), SHIPPED_GROUND),
        ((200, 101), SHIPPED_GROUND),
        ((200, 99), SHIPPED_GROUND),
    ];
    let at = |sc: &Scratch, p: (i32, i32)| {
        let mut px = SHIPPED_GROUND;
        for q in &sc.out[q0..] {
            if quad_covers(q, p.0, p.1) {
                px = add_sat(px, q.color);
            }
        }
        for h in &sc.halos[h0..] {
            px = add_sat(px, halo_add_at(h, p.0, p.1));
        }
        px
    };
    assert!(
        y(at(&sc, (200, 100))) <= ceil,
        "the centre is lawful as laid"
    );
    assert!(
        y(at(&sc, (203, 100))) > ceil,
        "the stub pixel is over as laid"
    );
    {
        let mut f = sc.frame();
        settle_under_ceiling(relative_luminance_table(), &mut f, q0, h0, &watch);
    }
    for &(p, _) in &watch {
        assert!(
            y(at(&sc, p)) <= ceil,
            "({}, {}) is at Y {:.1} after the closer",
            p.0,
            p.1,
            y(at(&sc, p))
        );
    }
    assert!(
        y(at(&sc, (203, 100))) >= ceil - 2.0,
        "the closer dimmed past the ceiling: the stub pixel is at Y {:.1}",
        y(at(&sc, (203, 100)))
    );
    let k = lum(sc.halos[h0].color) as f32 / lum(halo_before) as f32;
    assert!(k > 0.3 && k < 1.0, "the factor {k} is not a dimming");
    for (q, &was) in sc.out[q0..].iter().zip(&before) {
        for sh in [16, 8, 0] {
            let (now, then) = (((q.color >> sh) & 0xff) as f32, ((was >> sh) & 0xff) as f32);
            assert!(
                (now - then * k).abs() <= 1.0,
                "a lay carries {now} where the factor {k} says {}",
                then * k
            );
        }
    }
    assert!(chroma(sc.halos[h0].color) > 0, "the halo lost its colour");
    assert_eq!(sc.out[0].color, 0x0030_3030, "the earlier quad moved");

    let mut sc = Scratch::default();
    push_fx_rect(
        &mut sc.out,
        g,
        200,
        100,
        1,
        1,
        premul_rgb(TINT_WHITE_RGB, 40),
    );
    push_fx_rect(&mut sc.out, g, 203, 100, 2, 1, premul_rgb(yellow, 30));
    push_halo_quads(
        &mut sc.halos,
        g,
        usize::MAX,
        (200, 100),
        (15, 15),
        premul_rgb(yellow, 20),
        HaloMode::Add,
    );
    let (out, halos) = (sc.out.clone(), sc.halos.clone());
    {
        let mut f = sc.frame();
        settle_under_ceiling(relative_luminance_table(), &mut f, 0, 0, &watch);
    }
    assert!(sc.out.iter().zip(&out).all(|(a, b)| a.color == b.color));
    assert!(sc.halos.iter().zip(&halos).all(|(a, b)| a.color == b.color));
}

#[test]
fn the_aurora_rises_with_momentum_and_never_enters_a_glyph_row() {
    let c = cfg(true);
    let t0 = Instant::now();
    let ribbon = Ribbon::new();
    let g = geom();
    let run = || -> Vec<(u16, f32, Vec<RainHalo>)> {
        let mut sky = Stardust::new();
        let mut above = [false; 120];
        above[25..=30].fill(true);
        sky.probe_mut().probe_row(4, &above);
        let mut frames = Vec::new();
        for k in 0..40u16 {
            let at = key_at(t0, k);
            let mut cx = ctx(at, &c, (5, 10 + k));
            cx.disp = disp_at_12cps(k);
            cx.birth_disp = cx.disp;
            cx.caret_t = 0.0;
            sky.budget.refill(at);
            sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
            let mut sc = Scratch::default();
            sky.emit(&cx, &mut sc.frame());
            frames.push((k, cx.disp, veils(&sc)));
        }
        frames
    };
    let row5_top = i32::from(g.origin_y) + 5 * g.ch as i32;
    let inked =
        (i32::from(g.origin_x) + 25 * g.cw as i32)..(i32::from(g.origin_x) + 31 * g.cw as i32);
    let red = run();
    assert!(red[0].2.is_empty(), "a cold key laid a veil");
    let peak = |vs: &[RainHalo]| vs.iter().map(|h| lum(h.color)).max().unwrap_or(0);
    let plain = peak(&red[12].2);
    let full = peak(&red[39].2);
    assert!(
        full > plain && plain > 0,
        "at spine {} the veil is {full}, at spine {} {plain}: it does not rise",
        red[39].1,
        red[12].1
    );
    for (k, _, vs) in &red {
        for h in vs {
            let (y0, y1) = (i32::from(h.y), i32::from(h.y) + i32::from(h.h));
            assert!(
                y1 <= row5_top && y0 >= row5_top - g.ch as i32 && h.row == 4,
                "key {k}: a veil at y {y0}..{y1} enters a glyph row (row 5 top {row5_top})"
            );
            assert!(
                i32::from(h.h) <= (AURORA_TALL_CH * g.ch as f32).ceil() as i32,
                "key {k}: a veil {} px tall is over 0.35 ch",
                h.h
            );
            let (x0, x1) = (i32::from(h.x), i32::from(h.x) + i32::from(h.w));
            assert!(
                x1 - x0 <= g.cw as i32 && (x0 - i32::from(g.origin_x)) % g.cw as i32 == 0,
                "key {k}: a veil at x {x0}..{x1} is not one cell's own column"
            );
            assert!(
                x1 <= inked.start || x0 >= inked.end,
                "key {k}: a veil at x {x0}..{x1} sits over an inked sky cell"
            );
        }
    }
}

/// The aurora keeps off a filled cell of the row it lies in (a powerline segment's bar), which
/// the sky probe sees as free. The control keeps nothing out, and its veils lie on the cells.
#[test]
fn the_aurora_keeps_off_a_filled_cell_above() {
    let c = cfg(true);
    let t0 = Instant::now();
    let ribbon = Ribbon::new();
    let g = geom();
    let (cw, ch) = (g.cw as i32, g.ch as i32);
    let bar = (
        i32::from(g.origin_x) + 20 * cw,
        i32::from(g.origin_x) + 36 * cw,
        i32::from(g.origin_y) + 4 * ch,
        i32::from(g.origin_y) + 5 * ch,
    );
    let on_bar = |kept: bool| -> usize {
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        if kept {
            sky.keep_out.set([bar]);
        }
        let mut n = 0;
        for k in 0..40u16 {
            let at = key_at(t0, k);
            let mut cx = ctx(at, &c, (5, 10 + k));
            cx.disp = disp_at_12cps(k);
            cx.birth_disp = cx.disp;
            cx.caret_t = 0.0;
            sky.budget.refill(at);
            sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
            let mut sc = Scratch::default();
            sky.emit(&cx, &mut sc.frame());
            n += veils(&sc)
                .iter()
                .filter(|h| {
                    let (x0, y0) = (i32::from(h.x), i32::from(h.y));
                    let (x1, y1) = (x0 + i32::from(h.w), y0 + i32::from(h.h));
                    x0 < bar.1 && bar.0 < x1 && y0 < bar.3 && bar.2 < y1
                })
                .count();
        }
        n
    };
    assert!(
        on_bar(false) > 0,
        "control: the free sky's veils lie on the bar"
    );
    assert_eq!(
        on_bar(true),
        0,
        "a veil tints a filled cell of the row above"
    );
}

/// The aurora keeps off a lettered or filled cell's box but leaves a word icon's to the face's
/// shade (`CursorGlow::shade_word_icons`): its veils lie in the icon box's columns, and with
/// the box cut as a filled cell's (the negative control) none does.
#[test]
fn the_aurora_leaves_a_word_icon_to_its_face_s_shade() {
    let c = cfg(true);
    let t0 = Instant::now();
    let ribbon = Ribbon::new();
    let g = geom();
    let (cw, ch) = (g.cw as i32, g.ch as i32);
    let icon = (
        i32::from(g.origin_x) + 20 * cw,
        i32::from(g.origin_x) + 24 * cw,
        i32::from(g.origin_y) + 4 * ch,
        i32::from(g.origin_y) + 5 * ch,
    );
    let in_box = |as_cell: bool| -> usize {
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        if as_cell {
            sky.keep_out.set([icon]);
        } else {
            sky.keep_out.set_split([icon], []);
        }
        let mut n = 0;
        for k in 0..40u16 {
            let at = key_at(t0, k);
            let mut cx = ctx(at, &c, (5, 10 + k));
            cx.disp = disp_at_12cps(k);
            cx.birth_disp = cx.disp;
            cx.caret_t = 0.0;
            sky.budget.refill(at);
            sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
            let mut sc = Scratch::default();
            sky.emit(&cx, &mut sc.frame());
            n += veils(&sc)
                .iter()
                .filter(|h| {
                    let (x0, y0) = (i32::from(h.x), i32::from(h.y));
                    let (x1, y1) = (x0 + i32::from(h.w), y0 + i32::from(h.h));
                    x0 < icon.1 && icon.0 < x1 && y0 < icon.3 && icon.2 < y1
                })
                .count();
        }
        n
    };
    assert!(
        in_box(false) > 0,
        "the aurora's veils are dropped whole in a word icon's box"
    );
    assert_eq!(
        in_box(true),
        0,
        "control: cut as a filled cell, the box still takes a veil"
    );
}

#[test]
fn the_aurora_is_not_drawn_on_paper() {
    let t0 = Instant::now();
    let ribbon = Ribbon::new();
    let veiled = |dark: bool| -> usize {
        let mut c = cfg(dark);
        if !dark {
            (c.theme_fg, c.theme_bg) = (0x001F_2328, 0x00FF_FFFF);
        }
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let mut n = 0;
        for k in 0..40u16 {
            let at = key_at(t0, k);
            let mut cx = ctx(at, &c, (5, 10 + k));
            cx.disp = disp_at_12cps(k);
            cx.birth_disp = cx.disp;
            cx.caret_t = 0.0;
            sky.budget.refill(at);
            sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
            let mut sc = Scratch::default();
            sky.emit(&cx, &mut sc.frame());
            let g = geom();
            n += sc
                .halos
                .iter()
                .filter(|h| {
                    f32::from(h.rx) >= AURORA_REACH_CW * g.cw as f32 - 1.0
                        && h.rx > h.ry
                        && i32::from(h.w) <= g.cw as i32
                        && (i32::from(h.x) - i32::from(g.origin_x)) % g.cw as i32 == 0
                        && i32::from(h.h) <= (AURORA_TALL_CH * g.ch as f32).ceil() as i32
                })
                .count();
        }
        n
    };
    assert!(veiled(true) > 0, "control: the dark aurora draws");
    assert_eq!(veiled(false), 0, "a veil strip on paper");
}

#[test]
fn the_aurora_leaves_with_the_ribbon() {
    let c = cfg(true);
    let t0 = Instant::now();
    let ribbon = Ribbon::new();
    let g = geom();
    let hot = |sky: &mut Stardust, keys: u16| -> Instant {
        let mut last = t0;
        for k in 0..keys {
            last = key_at(t0, k);
            let mut cx = ctx(last, &c, (5, 10 + k));
            cx.disp = 1.0;
            cx.birth_disp = 1.0;
            cx.caret_t = 0.0;
            sky.budget.refill(last);
            sky.on_event(&typed(TypedClass::Glyph), last, &cx, &ribbon);
        }
        last
    };
    let peak_at = |sky: &mut Stardust, now: Instant| -> u32 {
        let mut sc = Scratch::default();
        sky.emit(&ctx(now, &c, (5, 29)), &mut sc.frame());
        veils(&sc).iter().map(|h| lum(h.color)).max().unwrap_or(0)
    };
    let mut sky = Stardust::new();
    sky.probe_mut().probe_row(4, &[false; 120]);
    let last = hot(&mut sky, 20);
    let at = |ms: u64| last + Duration::from_millis(ms);
    let (p0, p900) = (peak_at(&mut sky, at(0)), peak_at(&mut sky, at(900)));
    assert!(p0 > 0, "no veil on the last key");
    assert_eq!(p900, 0, "the veil outlived the ribbon's grace");
    assert!(
        (RETRACT_START_S * 1000.0) as u64 >= 900,
        "the fixture's 900 ms is not before the retract"
    );
    assert!(sky.at_rest(), "the veil is still in the pool");
    assert!(sky.next_change_deadline(at(900)).is_none());

    let mut sky = Stardust::new();
    sky.probe_mut().probe_row(4, &[false; 120]);
    let last = hot(&mut sky, 10);
    let erase = last + Duration::from_millis(20);
    sky.on_event(&Event::Erase, erase, &ctx(erase, &c, (5, 19)), &ribbon);
    let mut sc = Scratch::default();
    let done = erase + Duration::from_millis(240);
    sky.emit(&ctx(done, &c, (5, 19)), &mut sc.frame());
    let col_of = |h: &RainHalo| (i32::from(h.x) - i32::from(g.origin_x)) / g.cw as i32;
    let cols: std::collections::BTreeSet<i32> = veils(&sc).iter().map(col_of).collect();
    assert!(
        !cols.contains(&19),
        "the erased cell's veil survived the ribbon's retract: {cols:?}"
    );
    assert!(
        cols.contains(&10),
        "a cell before the caret lost its veil to the Backspace: {cols:?}"
    );
}

#[test]
fn the_combo_ladder_pays_at_sixteen_thirty_two_and_sixty_four() {
    let c = cfg(true);
    let t0 = Instant::now();
    let ribbon = Ribbon::new();
    let mut sky = Stardust::new();
    sky.probe_mut().probe_row(4, &[false; 120]);
    type Paid = (StarClass, u32, bool);
    let mut rung: Vec<(u32, Option<Paid>)> = Vec::new();
    for k in 0..70u16 {
        let at = key_at(t0, k);
        let combo = u32::from(k) + 1;
        let mut cx = ctx(at, &c, (5, 10 + k));
        cx.disp = 0.9;
        cx.birth_disp = 0.9;
        cx.flow = Flow {
            heat: 1.0,
            combo,
            best: combo,
        };
        sky.budget.refill(at);
        sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
        let best = sky
            .live_iter()
            .filter(|s| s.born == at && s.lane == StarLane::Strike)
            .min_by_key(|s| s.class)
            .map(|s| (s.class, s.tint, s.gold));
        rung.push((combo, best));
    }
    let paid = |combo: u32| -> Paid {
        rung.iter()
            .find(|(n, _)| *n == combo)
            .and_then(|(_, s)| *s)
            .unwrap_or_else(|| panic!("key {combo} of an open run drew NO strike star at all"))
    };
    for n in [LADDER_M2_KEYS, LADDER_M2_KEYS * 3] {
        let (class, _, _) = paid(n);
        assert!(
            class <= StarClass::M2,
            "key {n} paid a {class:?}, not the ladder's m2 or better"
        );
    }
    let (class, tint, gold) = paid(LADDER_WHITE_KEYS);
    assert_eq!(class, StarClass::M2, "the 32nd key pays an m2");
    assert_eq!(
        tint, TINT_WHITE_RGB,
        "…and it is WHITE by rule, not by deal"
    );
    assert!(!gold, "a white rung carries no glints");
    let (class, tint, gold) = paid(LADDER_GOLD_KEYS);
    assert_eq!(class, StarClass::M1, "the 64th key pays a hero");
    assert_eq!(tint, TINT_GOLD_RGB, "…gold by rule");
    assert!(gold, "…and gold-ness is the class that carries glints");

    let plain = rung
        .iter()
        .filter(|(n, _)| !n.is_multiple_of(LADDER_M2_KEYS))
        .filter(|(_, s)| s.is_none_or(|(c, _, _)| c == StarClass::M3))
        .count();
    assert!(
        plain > 20,
        "only {plain} of the non-rung keys took a grain or nothing — the ladder is not a payout, the deal moved"
    );
}

#[test]
fn an_open_theme_holds_the_sky_open_through_a_dip() {
    let c = cfg(true);
    let t0 = Instant::now();
    let ribbon = Ribbon::new();
    let census = |disp: f32, heat: f32| -> (f32, f32, usize) {
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let mut born = 0usize;
        for k in 0..60u16 {
            let at = key_at(t0, k);
            let mut cx = ctx(at, &c, (5, 10 + k));
            cx.disp = disp;
            cx.birth_disp = disp;
            cx.flow = Flow {
                heat,
                combo: if heat > 0.0 { FLOW_ENTRY_KEYS } else { 0 },
                best: FLOW_ENTRY_KEYS,
            };
            sky.budget.refill(at);
            sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
            born += sky.live_iter().filter(|s| s.born == at).count();
        }
        let field: std::collections::BTreeSet<i32> = sky
            .live_iter()
            .filter(|s| s.lane == StarLane::Field)
            .map(col_of)
            .collect();
        let held = (53..69).filter(|col| field.contains(col)).count();
        (born as f32 / 60.0, held as f32 / 16.0, sky.live())
    };
    let dip = FIELD_HOT_DISP - 0.05;
    let cold = census(dip, 0.0);
    let flowing = census(dip, 1.0);
    let hot = census(0.9, 0.0);
    assert!(
        flowing.1 >= 0.999,
        "an open theme left {:.2} of the last sixteen cells without a field star",
        1.0 - flowing.1
    );
    assert!(
        flowing.1 > cold.1 + 0.2 && flowing.2 > cold.2,
        "an open theme bought no density in the dip: field-held {:.2} → {:.2}, live {} → {}",
        cold.1,
        flowing.1,
        cold.2,
        flowing.2
    );
    assert!(
        (flowing.1 - hot.1).abs() < 0.01 && flowing.2 == hot.2,
        "an open theme's sky at a dip (field-held {:.2}, live {}) is not the hot step's ({:.2}, {})",
        flowing.1,
        flowing.2,
        hot.1,
        hot.2
    );
    assert!(
        flowing.2 <= STAR_CAP,
        "the open sky ran past the live cap: {} of {STAR_CAP}",
        flowing.2
    );
}

#[test]
fn one_sky_star_per_cell_and_no_pile_outshines_the_caret() {
    let mut c = cfg(true);
    c.theme_bg = SHIPPED_GROUND;
    let t0 = Instant::now();
    let ribbon = Ribbon::new();
    let mut sky = Stardust::new();
    sky.probe_mut().probe_row(4, &[false; 120]);
    let ceil = STAR_LIGHT_CEIL * 255.0;
    let (mut worst, mut over) = (0.0f32, 0usize);
    let mut piles = Vec::new();
    for k in 0..40u16 {
        let at = key_at(t0, k);
        let mut cx = ctx(at, &c, (5, 10 + k));
        cx.disp = disp_at_12cps(k);
        cx.birth_disp = cx.disp;
        cx.caret_t = f32::from(k) / 40.0;
        sky.budget.refill(at);
        sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
        let mut sc = Scratch::default();
        sky.emit(&cx, &mut sc.frame());
        let mut cells = std::collections::BTreeMap::new();
        for s in sky.live_iter().filter(|s| s.lane.is_sky()) {
            *cells.entry(col_of(s)).or_insert(0usize) += 1;
        }
        piles.extend(
            cells
                .iter()
                .filter(|&(_, &n)| n > 1)
                .map(|(&col, &n)| (k, col, n)),
        );
        let peak = composite_over(&sc, SHIPPED_GROUND, 0, 94, 1080, 54)
            .iter()
            .map(|&p| relative_luminance(p) * 255.0)
            .fold(0.0, f32::max);
        over += usize::from(peak > ceil);
        worst = worst.max(peak);
    }
    assert!(
        piles.is_empty(),
        "a cell carries more than one sky star: {piles:?}"
    );
    assert!(
        over == 0,
        "{over} of 40 frames reach past the caret law's {ceil:.1} — worst Y {worst:.1}"
    );
}

#[test]
fn no_mark_crosses_ink_or_the_pane_edge_at_any_instant_under_either_band_anchor() {
    for tall in [true, false] {
        let mut c = cfg(true);
        c.ribbon_tall = tall;
        let t0 = Instant::now();
        let g = geom_cell(15, 28);
        let cw = g.cw as f32;
        let ribbon = Ribbon::new();
        let priced = stream_travel_px(stream_px_per_s_with_share(
            EXHAUST_SHARE_CW,
            cw,
            1000.0 / 12.0,
        )) * stream_unit(EXHAUST_THROW_MS);
        let sky_travel = stream_travel_px(stream_px_per_s(cw, 1000.0 / 12.0));
        let sweep = |sky: &Stardust, s: &Star, at: Instant, min_col: i32| -> f32 {
            let (x0, _) = s.pos(at, false);
            let mut x_end = x0;
            for step in 0..=250u64 {
                let now = at + Duration::from_millis(step * 2);
                if s.dead(now) {
                    break;
                }
                let (x, y) = s.pos(now, false);
                x_end = x;
                assert_eq!(
                    sky.probe().at_px(x, y, g),
                    Some(false),
                    "tall {tall}: a {:?} mark stood on ink or the unknown at ({x}, {y}), \
                     +{} ms",
                    s.lane,
                    step * 2
                );
                assert!(
                    px_col(x as f32, g) >= min_col,
                    "tall {tall}: a {:?} mark stood over column {} at +{} ms — the floor \
                     is column {min_col}",
                    s.lane,
                    px_col(x as f32, g),
                    step * 2
                );
                assert!(
                    x >= g.fx_left(),
                    "tall {tall}: a {:?} mark stood at x {x}, left of the effects box's {} \
                     px edge, +{} ms",
                    s.lane,
                    g.fx_left(),
                    step * 2
                );
                if s.lane == StarLane::Exhaust {
                    assert!(
                        in_sky_of_row(y as f32, 5, g),
                        "tall {tall}: a grain left the caret row's sky at y {y}, +{} ms",
                        step * 2
                    );
                }
            }
            (x0 - x_end) as f32
        };
        let mut sky = Stardust::new();
        let mut occupied = [false; 120];
        let (mut marks, mut grains, mut clamped, mut sky_clamped) = (0usize, 0, 0, 0);
        let mut lanes: Vec<StarLane> = Vec::new();
        let mut widest = 0.0f32;
        for k in 0..20u16 {
            let col = 40 + k;
            for slot in occupied.iter_mut().take(usize::from(col) - 1) {
                *slot = true;
            }
            sky.probe_mut().probe_row(4, &[false; 120]);
            sky.probe_mut().probe_row(band_row(&c), &occupied);
            let at = t0 + Duration::from_secs_f32(f32::from(k) / 12.0);
            let mut cx = ctx_in(g, at, &c, (5, col));
            cx.disp = 0.9;
            cx.birth_disp = 0.9;
            sky.budget.refill(at);
            sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
            let floor = (g.fx_left() + (i32::from(col) - 1) * g.cw as i32) as f32;
            for s in sky.live_iter().filter(|s| s.born == at) {
                marks += 1;
                if !lanes.contains(&s.lane) {
                    lanes.push(s.lane);
                }
                let drift = sweep(&sky, s, at, i32::from(col) - 1);
                widest = widest.max(drift);
                let (x0, _) = s.pos(at, false);
                if s.lane == StarLane::Exhaust {
                    grains += 1;
                    let travel = (x0 - s.rest().0) as f32;
                    assert!(
                        travel > 0.0,
                        "tall {tall}: a grain over an inked row did not move at all"
                    );
                    clamped += usize::from(travel < priced - 1.0);
                } else if x0 as f32 - sky_travel < floor {
                    sky_clamped += 1;
                }
            }
        }
        assert!(
            marks >= 20 && lanes.len() >= 2,
            "tall {tall}: the fixture carried {marks} marks of {} lanes — too thin to be a \
             census of the sky",
            lanes.len()
        );
        assert!(
            widest >= cw,
            "tall {tall}: the widest drift over the whole run was {widest:.1} px — under a \
             cell, so the sweep is not reading a moving sky at all"
        );
        assert!(
            grains >= 15,
            "tall {tall}: the inked fixture threw {grains} grains over twenty keys — the \
             clamp is dropping them, not shortening them"
        );
        assert_eq!(
            clamped, grains,
            "tall {tall}: only {clamped} of {grains} grains were clamped short of the \
             priced {priced:.1} px: the frontier is not what is stopping them"
        );
        assert!(
            sky_clamped > 0,
            "tall {tall}: no sky star was born within its travel of the ink — the sky's \
             clamp was not exercised"
        );
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        sky.probe_mut().probe_row(5, &[false; 120]);
        let (mut edge_grains, mut edge_sky) = (0usize, 0usize);
        for k in 0..8u16 {
            let at = t0 + Duration::from_secs_f32(f32::from(k) / 12.0);
            let mut cx = ctx_in(g, at, &c, (5, k));
            cx.disp = 0.9;
            cx.birth_disp = 0.9;
            sky.budget.refill(at);
            sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
            for s in sky.live_iter().filter(|s| s.born == at) {
                sweep(&sky, s, at, 0);
                let (x0, _) = s.pos(at, false);
                let travel = if s.lane == StarLane::Exhaust {
                    priced
                } else {
                    sky_travel
                };
                if (x0 as f32 - travel) < g.fx_left() as f32 {
                    if s.lane == StarLane::Exhaust {
                        edge_grains += 1;
                    } else {
                        edge_sky += 1;
                    }
                }
            }
        }
        assert!(
            edge_grains > 0 && edge_sky > 0,
            "tall {tall}: {edge_grains} grains and {edge_sky} sky stars were born within \
             their travel of the pane's edge — the edge clamp was not exercised"
        );
    }
}

#[test]
fn the_sky_is_at_rest_one_star_life_after_the_last_key() {
    let c = cfg(true);
    let t0 = Instant::now();
    let (sky, _, _) = exhaust_run(12.0, 24, 30, 0, &c, t0);
    let last = t0 + Duration::from_secs_f32(23.0 / 12.0);
    let early = last + Duration::from_millis(100);
    let later = last + Duration::from_millis(200);
    let streaming = sky
        .live_iter()
        .filter(|s| s.pos(early, false).0 != s.pos(later, false).0)
        .count();
    assert!(
        streaming > 0,
        "nothing in the sky is streaming 100 ms after the last key"
    );
    let rest = last + Duration::from_secs_f32(STREAM_END_MS / 1000.0);
    for s in sky.live_iter() {
        let at_rest = s.pos(rest, false);
        for ms in [1u64, 50, 200, 1_000, 3_000] {
            let p = s.pos(rest + Duration::from_millis(ms), false);
            assert_eq!(
                p.0, at_rest.0,
                "a star still moves {ms} ms after the sky should be at rest ({:?} → {:?})",
                at_rest, p
            );
        }
        assert!(
            s.next_stream_step_s(rest).is_none(),
            "a star at rest still offers a stream step"
        );
    }
}

#[test]
fn the_stream_step_is_solved_not_polled() {
    let t0 = Instant::now();
    let mut s = star(StarClass::M2, StarLane::Strike, t0, TINT_WHITE_RGB);
    s.stream = stream_travel_px(stream_px_per_s(15.0, 1000.0 / 12.0));
    let mut steps = 0usize;
    let mut age_ms = 0u64;
    while age_ms < STREAM_END_MS as u64 + 100 {
        let now = t0 + Duration::from_millis(age_ms);
        let Some(dt) = s.next_stream_step_s(now) else {
            break;
        };
        let at = now + Duration::from_secs_f32(dt);
        let before = s.pos(at - Duration::from_millis(1), false).0;
        let after = s.pos(at + Duration::from_millis(1), false).0;
        assert!(
            before != after,
            "the solved step at +{dt:.4} s from {age_ms} ms moves nothing ({before} → {after})"
        );
        assert!(dt >= 0.0, "a negative step {dt} from {age_ms} ms");
        steps += 1;
        age_ms = (age_ms as f32 + dt * 1000.0).floor() as u64 + 2;
    }
    assert!(
        steps == s.stream.round() as usize,
        "{steps} solved steps for a {:.1} px travel — one per pixel expected",
        s.stream
    );
    let done = t0 + Duration::from_secs_f32(STREAM_END_MS / 1000.0);
    assert!(
        s.next_stream_step_s(done).is_none(),
        "a step offered past the drift's end"
    );
    assert_eq!(
        s.pos(done, false).0,
        200 - s.stream.round() as i32,
        "the star did not travel its whole priced distance"
    );
}

fn exhaust_run(
    cps: f32,
    keys: u16,
    col0: u16,
    ink_to: usize,
    c: &Config,
    t0: Instant,
) -> (Stardust, Geom, Vec<(Star, Instant)>) {
    let g = geom_cell(15, 28);
    let ribbon = Ribbon::new();
    let mut sky = Stardust::new();
    let mut occupied = [false; 120];
    for slot in occupied.iter_mut().take(ink_to) {
        *slot = true;
    }
    sky.probe_mut().probe_row(4, &[false; 120]);
    sky.probe_mut().probe_row(band_row(c), &occupied);
    let mut born = Vec::new();
    for k in 0..keys {
        let at = t0 + Duration::from_secs_f32(f32::from(k) / cps);
        let mut cx = ctx_in(g, at, c, (5, col0 + k));
        cx.disp = 0.9;
        cx.birth_disp = 0.9;
        sky.budget.refill(at);
        sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
        born.extend(
            sky.live_iter()
                .filter(|s| s.born == at && s.lane == StarLane::Exhaust)
                .map(|s| (*s, at)),
        );
    }
    (sky, g, born)
}

/// The row a sky star of row 5 is born in: the row above, under either anchor.
const fn band_row(_: &Config) -> i32 {
    4
}

fn exhaust_census(
    class: TypedClass,
    cps: f32,
    disp: f32,
    dark: bool,
    reduced: bool,
    keys: u16,
    t0: Instant,
) -> (usize, usize, f32, usize, usize) {
    let g = geom_cell(15, 28);
    let ribbon = Ribbon::new();
    let mut c = cfg(dark);
    c.reduced_motion = reduced;
    let mut sky = Stardust::new();
    sky.probe_mut().probe_row(4, &[false; 120]);
    let (mut n, mut m2, mut inked) = (0usize, 0usize, 0usize);
    for k in 0..keys {
        let at = t0 + Duration::from_secs_f32(f32::from(k) / cps);
        let mut cx = ctx_in(g, at, &c, (5, 40 + k));
        cx.disp = disp;
        cx.birth_disp = disp;
        sky.budget.refill(at);
        sky.on_event(&typed(class), at, &cx, &ribbon);
        for s in sky
            .live_iter()
            .filter(|s| s.born == at && s.lane == StarLane::Exhaust)
        {
            n += 1;
            m2 += usize::from(s.class == StarClass::M2);
            inked += usize::from(s.inks(&c));
        }
    }
    let glints = sky.take_glints().count();
    (n, m2, sky.budget.tokens(), glints, inked)
}

fn exhaust_duty_in(c: &Config, row_4: CellInk, cps: f32, keys: u16) -> (u32, u32, usize, usize) {
    let t0 = Instant::now();
    let g = geom_cell(15, 28);
    let ribbon = Ribbon::new();
    let mut sky = Stardust::new();
    sky.probe_mut().probe_row_ink(4, &[row_4; 120]);
    sky.probe_mut().probe_row(5, &[false; 120]);
    let span_ms = (1000.0 * f32::from(keys - 1) / cps) as u64 + 300;
    let (mut brisk_ms, mut bare_ms) = (0u64, 0u64);
    let (mut max_live, mut evicted, mut prev_fin) = (0usize, 0usize, 0usize);
    let mut next_key = 0u16;
    for ms in 0..span_ms {
        let now = t0 + Duration::from_millis(ms);
        while next_key < keys && t0 + Duration::from_secs_f32(f32::from(next_key) / cps) <= now {
            let mut cx = ctx_in(g, now, c, (5, 20 + (next_key % 90)));
            cx.disp = 0.9;
            cx.birth_disp = 0.9;
            sky.budget.refill(now);
            sky.on_event(&typed(TypedClass::Glyph), now, &cx, &ribbon);
            next_key += 1;
            let fin = sky.live_iter().filter(|s| s.finish.is_some()).count();
            evicted += fin.saturating_sub(prev_fin);
            prev_fin = fin;
            max_live = max_live.max(sky.live_iter().filter(|s| s.finish.is_none()).count());
        }
        brisk_ms += u64::from(sky.brisk(now));
        bare_ms += u64::from(
            sky.stars
                .iter()
                .any(|s| s.lane != StarLane::Exhaust && s.brisk(now))
                || sky.veil.iter().any(|v| v.age_s(now) < EDGE_IN_S),
        );
    }
    let pct = |ms: u64| (ms * 100 / span_ms.max(1)) as u32;
    (pct(brisk_ms), pct(bare_ms), max_live, evicted)
}

#[test]
fn the_exhaust_never_chimes_skips_a_space_and_deals_its_m2_only_at_a_hot_spine() {
    let t0 = Instant::now();
    let hot = exhaust_census(TypedClass::Glyph, 8.0, 0.9, true, false, 12, t0);
    assert_eq!(
        hot.0, 11,
        "a hot 8 cps hand threw {} grains over twelve keys, not one each from the second",
        hot.0
    );
    let no_grain = exhaust_census(TypedClass::Glyph, 8.0, 0.9, true, true, 12, t0);
    assert_eq!(
        no_grain.0, 0,
        "the no-grain control (reduced motion) sowed {} grains",
        no_grain.0
    );
    assert!(
        (hot.2 - no_grain.2).abs() < 1e-6,
        "eleven grains cost the glint bucket {} tokens: the exhaust is a THROW and spends \
         none (§13 — one keystroke is one melody step)",
        no_grain.2 - hot.2
    );
    assert!(
        !StarLane::Exhaust.may_chime(),
        "the exhaust lane may chime: ONE KEYSTROKE IS ONE MELODY STEP, and the key that \
         threw the grain already has its tine (§13, §19.2)"
    );
    let space = exhaust_census(TypedClass::Space, 8.0, 0.9, true, false, 12, t0);
    assert_eq!(
        space.0, 0,
        "twelve Spaces at a hot spine threw {} grains: a Space lays bed, it does not strike",
        space.0
    );
    assert!(
        (STREAM_DISP..FIELD_HOT_DISP).contains(&0.6),
        "0.6 is no longer between STREAM_DISP {STREAM_DISP} and FIELD_HOT_DISP \
         {FIELD_HOT_DISP}: the warm-but-not-hot band this clause needs has moved"
    );
    let warm = exhaust_census(TypedClass::Glyph, 8.0, 0.6, true, false, 60, t0);
    assert_eq!(
        warm.0, 59,
        "a warm hand (spine 0.6) threw {} grains over sixty keys, not one each from the \
         second: the exhaust's own gate is STREAM_DISP, not FIELD_HOT_DISP",
        warm.0
    );
    assert_eq!(
        warm.1, 0,
        "a warm hand (spine 0.6, under FIELD_HOT_DISP {FIELD_HOT_DISP}) dealt {} m2s: the \
         m2 is the HOT deal",
        warm.1
    );
    let dark = exhaust_census(TypedClass::Glyph, 8.0, 0.9, true, false, 60, t0);
    assert_eq!(
        dark.0, 59,
        "sixty keys on a dark theme threw {} grains, not one each from the second",
        dark.0
    );
    assert!(
        dark.1 > 0,
        "sixty grains at a hot spine dealt no m2 at all (1 in {EXHAUST_M2_IN}): the warm \
         clause above is vacuous"
    );
    let light = exhaust_census(TypedClass::Glyph, 8.0, 0.9, false, false, 60, t0);
    assert_eq!(
        light.0, dark.0,
        "the light theme threw {} grains where the dark threw {}: the thinning is the \
         ink's, not the throw's",
        light.0, dark.0
    );
    assert_eq!(dark.4, dark.0, "a dark grain went uninked");
    let share = light.4 as f32 / light.0 as f32;
    eprintln!(
        "the light theme inked {} of its {} grains — {share:.2}",
        light.4, light.0
    );
    assert!(
        (0.45..=0.80).contains(&share),
        "the light share is {share:.2}, not about {LIGHT_COUNT_SCALE}"
    );
}

#[test]
fn the_exhaust_costs_the_frame_train_its_own_window_and_no_more() {
    let c = cfg(true);
    let t0 = Instant::now();
    let (sky, _, born) = exhaust_run(12.0, 16, 40, 0, &c, t0);
    let last = born.last().expect("the run threw grains").1;
    let at = |ms: u64| last + Duration::from_millis(ms);
    let edge_in_ms = (EDGE_IN_S * 1000.0).ceil() as u64 + 2;
    assert!(
        sky.brisk(at(edge_in_ms)),
        "the sky went quiet {edge_in_ms} ms after the key: the grain is not holding the train"
    );
    assert!(
        sky.brisk(at(EXHAUST_THROW_MS as u64 - 1)),
        "the sky went quiet before the grain's throw was over"
    );
    assert!(
        !sky.brisk(at(EXHAUST_THROW_MS as u64 + 1)),
        "the sky is still brisk after the last grain's throw: the window does not close"
    );
    let (_, gf, fast_born) = exhaust_run(30.0, 60, 20, 0, &c, t0);
    let cw = gf.cw as f32;
    let capped = WALK_FAST_CELLS * cw * stream_unit(EXHAUST_THROW_MS);
    let uncapped = stream_travel_px(stream_px_per_s_with_share(
        EXHAUST_SHARE_CW,
        cw,
        1000.0 / 30.0,
    )) * stream_unit(EXHAUST_THROW_MS);
    assert!(
        uncapped > capped + cw,
        "at 30 cps the priced travel is {uncapped:.1} px and the cap {capped:.1}: the cap \
         is not binding, so the clause below measures nothing"
    );
    assert!(!fast_born.is_empty(), "the 30 cps fixture threw nothing");
    for (s, at) in &fast_born {
        let travel = (s.pos(*at, false).0 - s.rest().0) as f32;
        assert!(
            (travel - capped).abs() < 1.5,
            "a 30 cps grain travelled {travel:.1} px where the cap is {capped:.1} \
             (the priced, uncapped travel is {uncapped:.1})"
        );
    }
    let expect = |cps: f32| -> (u32, u32) {
        let ioi = 1000.0 / cps;
        let edge = EDGE_IN_S * 1000.0;
        let span = (1000.0 * 59.0 / cps).floor() + 300.0;
        let held = if ioi < EXHAUST_THROW_MS {
            58.0 * ioi + EXHAUST_THROW_MS
        } else {
            59.0 * EXHAUST_THROW_MS
        } + edge;
        let bare = 60.0 * edge.min(ioi);
        ((held * 100.0 / span) as u32, (bare * 100.0 / span) as u32)
    };
    for dark in [true, false] {
        let c = cfg(dark);
        let theme = if dark { "dark" } else { "light" };
        for cps in [4.0f32, 12.0] {
            let (on, off, live, evicted) = exhaust_duty_in(&c, CellInk::Blank, cps, 60);
            let (want_on, want_off) = expect(cps);
            eprintln!(
                "{theme}, {cps} cps, 60 keys: brisk {on}% of the session with the exhaust \
                 (arithmetic {want_on}%), {off}% without it (arithmetic {want_off}%); \
                 {live} live of {STAR_CAP}, {evicted} evictions"
            );
            assert_eq!(
                evicted, 0,
                "{theme}, {cps} cps evicted {evicted} stars, so the no-exhaust baseline is \
                 not a counterfactual: the exhaust displaced a star the baseline keeps"
            );
            assert!(
                on.abs_diff(want_on) <= 2,
                "{theme}, {cps} cps holds the frame train {on}% of the session where its \
                 window arithmetic says {want_on}%: the exhaust costs more, or less, than \
                 its window"
            );
            assert!(
                off.abs_diff(want_off) <= 2,
                "{theme}, {cps} cps was brisk {off}% of the session with no grain in the \
                 pool, where the edge-ins alone say {want_off}%: the BASELINE has moved"
            );
        }
    }
    for (label, disp, cps, reduced) in [
        ("reduced motion", 0.9f32, 12.0f32, true),
        ("a cold spine", 0.3, 12.0, false),
        ("a 2 cps stroll", 0.9, 2.0, false),
    ] {
        let mut c = cfg(true);
        c.reduced_motion = reduced;
        let g = geom_cell(15, 28);
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let mut last = t0;
        for k in 0..16u16 {
            last = t0 + Duration::from_secs_f32(f32::from(k) / cps);
            let mut cx = ctx_in(g, last, &c, (5, 40 + k));
            cx.disp = disp;
            cx.birth_disp = disp;
            sky.budget.refill(last);
            sky.on_event(&typed(TypedClass::Glyph), last, &cx, &ribbon);
        }
        assert_eq!(
            sky.live_iter()
                .filter(|s| s.lane == StarLane::Exhaust)
                .count(),
            0,
            "{label} sowed an exhaust grain"
        );
        assert!(
            reduced || !sky.brisk(last + Duration::from_millis(edge_in_ms)),
            "{label} owed the engine a frame-cadence wake {edge_in_ms} ms after the key, \
             past the field's own edge-in"
        );
    }
}

#[test]
fn the_exhaust_needs_a_blank_cell_behind_the_caret() {
    let t0 = Instant::now();
    let edge_in_ms = (EDGE_IN_S * 1000.0).ceil() as u64 + 2;
    let run = |c: &Config, probe: &dyn Fn(&mut Stardust, u16)| {
        let g = geom_cell(15, 28);
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        let mut grains = 0usize;
        let mut last = t0;
        for k in 0..16u16 {
            let col = 40 + k;
            probe(&mut sky, col);
            last = t0 + Duration::from_secs_f32(f32::from(k) / 12.0);
            let mut cx = ctx_in(g, last, c, (5, col));
            cx.disp = 0.9;
            cx.birth_disp = 0.9;
            sky.budget.refill(last);
            sky.on_event(&typed(TypedClass::Glyph), last, &cx, &ribbon);
            grains += sky
                .live_iter()
                .filter(|s| s.born == last && s.lane == StarLane::Exhaust)
                .count();
        }
        (grains, sky.brisk(last + Duration::from_millis(edge_in_ms)))
    };
    let text_to = |sky: &mut Stardust, col: u16, gap: u16| {
        let mut cells = [false; 120];
        for slot in cells.iter_mut().take(usize::from(col.saturating_sub(gap))) {
            *slot = true;
        }
        sky.probe_mut().probe_row(5, &cells);
    };
    let tall = cfg(true);
    let (grains, brisk) = run(&tall, &|sky, col| {
        let mut above = [true; 120];
        above[usize::from(col)] = false;
        sky.probe_mut().probe_row(4, &above);
        text_to(sky, col, 0);
    });
    assert_eq!(
        grains, 0,
        "tall with the row above inked but for the caret's own column threw {grains} \
         grains: every one of them was a sub-cell twitch"
    );
    assert!(
        !brisk,
        "tall with the row above inked but for the caret's own column owed the engine a \
         frame-cadence wake {edge_in_ms} ms after the key, with nothing thrown to show for it"
    );
    let (grains, _) = run(&tall, &|sky, col| {
        sky.probe_mut().probe_row(4, &[false; 120]);
        text_to(sky, col, 0);
    });
    assert_eq!(
        grains, 15,
        "tall under a blank row threw {grains} grains, not one per key from the second"
    );
    // The underline's sky is the row above's foot, as tall's is
    // (`sky_birth_px`), so its grains are tall's.
    let mut under = cfg(true);
    under.ribbon_tall = false;
    let (grains, _) = run(&under, &|sky, col| {
        sky.probe_mut().probe_row(4, &[false; 120]);
        text_to(sky, col, 0);
    });
    assert_eq!(
        grains, 15,
        "underline under a blank row threw {grains} grains, not one per key from the second"
    );
    let (grains, brisk) = run(&under, &|sky, col| {
        let mut above = [true; 120];
        above[usize::from(col)] = false;
        sky.probe_mut().probe_row(4, &above);
        text_to(sky, col, 0);
    });
    assert_eq!(
        grains, 0,
        "underline with the row above inked but for the caret's own column threw {grains} \
         grains"
    );
    assert!(
        !brisk,
        "underline with the row above inked owed a frame-cadence wake {edge_in_ms} ms after \
         the key"
    );
    let glyph_under_caret = |sky: &mut Stardust, col: u16| {
        let mut cells = [true; 120];
        cells[usize::from(col) - 1] = false;
        sky.probe_mut().probe_row(4, &[false; 120]);
        sky.probe_mut().probe_row(5, &cells);
    };
    for (c, side) in [(&under, "underline"), (&tall, "tall")] {
        assert_eq!(
            run(c, &glyph_under_caret).0,
            15,
            "{side} over a blank row threw no grain past a caret over a glyph"
        );
    }
}

#[test]
fn the_exhaust_flies_along_a_rule_row_in_the_sky_zone_under_the_stroke() {
    let c = cfg(true);
    let t0 = Instant::now();
    let g = geom_cell(15, 28);
    let (cw, ch) = (g.cw as f32, g.ch as f32);
    type Born = Vec<(Star, Instant)>;
    let run = |row_4: &[CellInk; 120]| -> (Stardust, Born, Born) {
        let ribbon = Ribbon::new();
        let mut sky = Stardust::new();
        let (mut grains, mut stars) = (Vec::new(), Vec::new());
        for k in 0..60u16 {
            let col = 20 + k;
            let mut line = [false; 120];
            for slot in line.iter_mut().take(usize::from(col)) {
                *slot = true;
            }
            sky.probe_mut().probe_row_ink(4, row_4);
            sky.probe_mut().probe_row(5, &line);
            let at = t0 + Duration::from_secs_f32(f32::from(k) / 12.0);
            let mut cx = ctx_in(g, at, &c, (5, col));
            cx.disp = 0.9;
            cx.birth_disp = 0.9;
            sky.budget.refill(at);
            sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
            for s in sky.live_iter().filter(|s| s.born == at) {
                if s.lane == StarLane::Exhaust {
                    grains.push((*s, at));
                } else if s.lane.is_sky() {
                    stars.push((*s, at));
                }
            }
        }
        (sky, grains, stars)
    };
    let travel = |v: &[(Star, Instant)]| -> Vec<f32> {
        v.iter()
            .map(|(s, at)| (s.pos(*at, false).0 - s.rest().0) as f32)
            .collect()
    };
    let band = rule_stroke_px(g.ch);
    let row_4_top = i32::from(g.origin_y) + 4 * g.ch as i32;
    let sample = |pool: &Stardust, marks: &[(Star, Instant)]| -> (usize, usize, usize, usize) {
        let (mut samples, mut in_band, mut on_ink, mut rests) = (0usize, 0usize, 0usize, 0usize);
        for (s, at) in marks {
            for step in 0..=1000u64 {
                let now = *at + Duration::from_millis(step * 2);
                if s.dead(now) {
                    break;
                }
                let (x, y) = s.pos(now, false);
                samples += 1;
                let ruled = u16::try_from(px_col(x as f32, g))
                    .is_ok_and(|col| pool.probe().rule_at(4, col));
                in_band += usize::from(
                    ruled && px_row(y as f32, g) == 4 && band.contains(&(y - row_4_top)),
                );
                on_ink += usize::from(pool.probe().at_px(x, y, g) != Some(false));
                assert!(
                    in_sky_of_row(y as f32, 5, g),
                    "a {:?} mark left the caret row's sky at y {y}, +{} ms",
                    s.lane,
                    step * 2
                );
            }
            let rest = s.rest();
            rests += usize::from(pool.probe().at_px(rest.0, rest.1, g) == Some(true));
        }
        (samples, in_band, on_ink, rests)
    };
    let (sky, grains, stars) = run(&[CellInk::Rule; 120]);
    let (_, blank_grains, blank_stars) = run(&[CellInk::Blank; 120]);
    let travels = travel(&grains);
    assert_eq!(
        grains.len(),
        59,
        "sixty keys under a rule row threw {} grains, not one per key from the second",
        grains.len()
    );
    assert_eq!(
        grains.len(),
        blank_grains.len(),
        "the rule row threw {} grains where the blank row threw {}",
        grains.len(),
        blank_grains.len()
    );
    for (k, t) in travels.iter().enumerate() {
        assert!(
            *t >= 3.0 * cw,
            "grain {k} ran {t:.1} px along the rule row — under three cells"
        );
    }
    let (streams, blank_streams) = (travel(&stars), travel(&blank_stars));
    let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len().max(1) as f32;
    let max = |v: &[f32]| v.iter().copied().fold(0.0f32, f32::max);
    assert!(
        streams.len() == blank_streams.len() && !streams.is_empty(),
        "the rule row bore {} sky stars where the blank row bore {}",
        streams.len(),
        blank_streams.len()
    );
    assert!(
        (mean(&streams) - mean(&blank_streams)).abs() <= 1.0
            && (max(&streams) - max(&blank_streams)).abs() <= 1.0,
        "the sky streams a mean {:.1} px (max {:.1}) along a rule row against {:.1} (max \
         {:.1}) over a blank row: the walk still calls a rule ink",
        mean(&streams),
        max(&streams),
        mean(&blank_streams),
        max(&blank_streams)
    );
    let (samples, in_band, on_ink, rests) = sample(&sky, &grains);
    let (star_samples, star_in_band, star_on_ink, star_rests) = sample(&sky, &stars);
    let min_rise = (0.5 * EXHAUST_RISE_CH * ch).round() as i32;
    let clamped = grains
        .iter()
        .filter(|(s, at)| s.pos(*at, false).1 - s.rest().1 < min_rise)
        .count();
    let full_lift = |s: &Star| {
        (-sky_lift_px_per_s(ch) / 1000.0 * s.class.life_s(s.lane) * 1000.0).round() as i32
    };
    let lifts = |want: &dyn Fn(i32, i32) -> bool| {
        stars
            .iter()
            .filter(|(s, at)| {
                s.lane == StarLane::Strike
                    && s.class != StarClass::M3
                    && want(s.pos(*at, false).1 - s.rest().1, full_lift(s))
            })
            .count()
    };
    let (held, whole) = (
        lifts(&|lift, full| lift < full),
        lifts(&|lift, full| lift == full),
    );
    assert!(samples > 100, "only {samples} flight samples: nothing flew");
    assert_eq!(
        in_band, 0,
        "{in_band} of {samples} flight samples had the core inside the rule's stroke band \
         {band:?} of row 4"
    );
    assert_eq!(
        on_ink, 0,
        "{on_ink} of {samples} flight samples stood on ink or the unknown"
    );
    assert_eq!(rests, 0, "{rests} grains came to rest on the rule's stroke");
    assert!(
        clamped > 0,
        "no grain rose under the smallest unclamped rise ({min_rise} px): the headroom \
         clamp was not exercised"
    );
    assert!(
        star_samples > 100,
        "only {star_samples} sky samples: no star lived"
    );
    assert_eq!(
        star_in_band, 0,
        "{star_in_band} of {star_samples} sky-star samples had the core inside the rule's \
         stroke band {band:?} of row 4: the strike's lift is not held under the stroke"
    );
    assert_eq!(
        star_on_ink, 0,
        "{star_on_ink} of {star_samples} sky-star samples stood on ink or the unknown"
    );
    assert_eq!(
        star_rests, 0,
        "{star_rests} sky stars came to rest on the rule's stroke"
    );
    assert!(
        held > 0 && whole > 0,
        "of the lifting strikes {held} lift less than their whole lift and {whole} lift it \
         all: the headroom is not being read per birth"
    );
    assert!(
        blank_stars
            .iter()
            .filter(|(s, _)| s.lane == StarLane::Strike && s.class != StarClass::M3)
            .all(|(s, at)| s.pos(*at, false).1 - s.rest().1 == full_lift(s)),
        "a strike over a blank row lifted less than its whole lift"
    );
    assert!(
        grains.iter().all(|(s, _)| s.class != StarClass::M1),
        "the exhaust dealt an m1 along the rule row"
    );
    let mut title = [CellInk::Blank; 120];
    title[..40].fill(CellInk::Rule);
    let (pool, grains, stars) = run(&title);
    let crossed = grains
        .iter()
        .chain(&stars)
        .filter(|(s, at)| {
            px_col(s.pos(*at, false).0 as f32, g) >= 40 && px_col(s.rest().0 as f32, g) < 40
        })
        .count();
    let (_, in_band, on_ink, rests) = sample(&pool, &grains);
    let (_, star_in_band, star_on_ink, star_rests) = sample(&pool, &stars);
    assert!(
        crossed > 0,
        "no mark born over a blank cell crossed onto the rule"
    );
    assert_eq!(
        (
            in_band,
            on_ink,
            rests,
            star_in_band,
            star_on_ink,
            star_rests
        ),
        (0, 0, 0, 0, 0, 0),
        "a mark crossing onto a rule that ends left of the caret entered its stroke: \
         grains {in_band} in band / {on_ink} on ink / {rests} rests, stars {star_in_band} / \
         {star_on_ink} / {star_rests}"
    );
}

#[test]
fn an_exhaust_grain_rests_by_the_probe_and_not_by_its_home_cell() {
    let c = cfg(true);
    let t0 = Instant::now();
    let g = geom_cell(15, 28);
    let mut sky = Stardust::new();
    sky.probe_mut().probe_row_ink(4, &[CellInk::Rule; 120]);
    let cx = ctx_in(g, t0, &c, (5, 40));
    let (x, _) = g.cell_center(4, 40);
    let row_top = f32::from(g.origin_y) + 4.0 * g.ch as f32;
    let y = row_top + (0.62 * g.ch as f32).round();
    for (lane, sown) in [(StarLane::Exhaust, false), (StarLane::Erase, true)] {
        let mut s = star(StarClass::M3, lane, t0, TINT_WHITE_RGB);
        s.x = x.round();
        s.y = y;
        let window = lane.throw_ms().expect("a throw lane");
        s.v0 = (-3.0 / window, -4.0 / window);
        let rest = s.rest();
        assert_eq!(
            sky.probe().at_px(rest.0, rest.1, g),
            Some(true),
            "{lane:?}: the fixture's rest {rest:?} is not on the rule's stroke"
        );
        assert_eq!(
            sky.settle(&mut s, false, &cx),
            sown,
            "{lane:?}: a rest on the stroke was {}",
            if sown { "dropped" } else { "sown" }
        );
        if sown {
            assert_eq!(s.rest(), rest, "{lane:?}: the exempt rest was re-aimed");
        }
    }
}

#[test]
fn the_exhaust_floors_its_spine_on_the_mend_s() {
    let c = cfg(true);
    let t0 = Instant::now();
    let g = geom_cell(15, 28);
    let ribbon = Ribbon::new();
    let run = |mended: bool| {
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &[false; 120]);
        let mut n = 0usize;
        for k in 0..12u16 {
            let col = 40 + k;
            let at = t0 + Duration::from_secs_f32(f32::from(k) / 8.0);
            let mut cx = ctx_in(g, at, &c, (5, col));
            cx.disp = 0.3;
            cx.birth_disp = 0.3;
            cx.mend = mended.then_some(Mend {
                at,
                disp: 0.9,
                row: 5,
                col: col - 1,
                deletes: 1,
            });
            sky.budget.refill(at);
            sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
            n += sky
                .live_iter()
                .filter(|s| s.born == at && s.lane == StarLane::Exhaust)
                .count();
        }
        n
    };
    let cold = run(false);
    assert_eq!(
        cold, 0,
        "a cold hand (spine 0.3) threw {cold} grains with no mend to floor it"
    );
    let mended = run(true);
    assert_eq!(
        mended, 11,
        "a cold hand mending at 0.9 threw {mended} grains, not one per key from the second: \
         the exhaust does not floor its spine on the mend's"
    );
}

#[test]
fn the_blank_frontiers_early_stop_is_the_whole_walks_answer() {
    let c = cfg(true);
    let t0 = Instant::now();
    let g = geom_cell(15, 28);
    let cw = g.cw as f32;
    let cx = ctx_in(g, t0, &c, (5, 60));
    let mut checked = 0usize;
    for pattern in 0..12u32 {
        let mut occupied = [false; 120];
        for (col, slot) in occupied.iter_mut().enumerate() {
            *slot = match pattern {
                0 => false,
                1 => true,
                p => hash01(mix32(col as u32 ^ (p << 16))) < 0.08 * p as f32,
            };
        }
        let mut sky = Stardust::new();
        sky.probe_mut().probe_row(4, &occupied);
        for caret in [0u16, 1, 2, 7, 30, 59, 60, 61, 119] {
            let (x, _) = g.cell_center(5, caret);
            let mut frontier = i32::from(caret);
            while frontier > 0 {
                let left = frontier - 1;
                if !occupied[left as usize] {
                    frontier = left;
                } else {
                    break;
                }
            }
            let floor_x = (g.fx_left() + frontier * g.cw as i32) as f32;
            for want in [0.4f32, 0.5, 3.0, 7.0 * cw, 16.0 * cw, 40.0 * cw] {
                let reference = if want.is_finite() && want >= 0.5 {
                    want.min(x - floor_x).max(0.0)
                } else {
                    0.0
                };
                let by = default_band_top(g, &c, 5) - 0.2 * g.ch as f32;
                let got = sky.blank_frontier_px(x, (by, by), want, &cx);
                assert!(
                    (got - reference).abs() < 1e-4,
                    "pattern {pattern}, caret {caret}, want {want}: the early stop \
                     answered {got} where the whole walk answers {reference}"
                );
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 12 * 9 * 6, "the census did not run in full");
}

fn hot_event(
    sky: &mut Stardust,
    ribbon: &Ribbon,
    world: (Geom, &Config),
    ev: &Event,
    at: Instant,
    caret: (u16, u16),
    mend: Option<Mend>,
) {
    let (g, c) = world;
    let mut cx = ctx_in(g, at, c, caret);
    cx.disp = 0.9;
    cx.birth_disp = 0.9;
    cx.mend = mend;
    sky.budget.refill(at);
    sky.on_event(ev, at, &cx, ribbon);
    let mut sc = Scratch::default();
    let mut f = sc.frame();
    sky.emit(&cx, &mut f);
}

#[expect(clippy::type_complexity, reason = "a fixture's five readings")]
fn mend_sky(
    typo: TypedClass,
    deletes: u16,
    fix_after_ms: u64,
    mended: bool,
    c: &Config,
    t0: Instant,
) -> (
    Stardust,
    Instant,
    (u16, u16),
    Option<((i32, i32), StarClass)>,
    Vec<StarClass>,
) {
    const SLOT_MS: u64 = 125;
    let g = geom_cell(15, 28);
    let ribbon = Ribbon::new();
    let mut sky = Stardust::new();
    sky.probe_mut().probe_row(4, &[false; 120]);
    let glyph = typed(TypedClass::Glyph);
    let mut last = t0;
    for k in 0..8u16 {
        last = t0 + Duration::from_millis(SLOT_MS * u64::from(k));
        let key = if k == 7 { typed(typo) } else { glyph };
        hot_event(&mut sky, &ribbon, (g, c), &key, last, (5, 5 + k), None);
    }
    let mut caret = 12u16;
    for _ in 0..deletes {
        last += Duration::from_millis(SLOT_MS);
        caret -= 1;
        hot_event(
            &mut sky,
            &ribbon,
            (g, c),
            &Event::Erase,
            last,
            (5, caret),
            None,
        );
    }
    let erased = (5u16, caret);
    let fix = last + Duration::from_millis(fix_after_ms);
    let retiring = sky
        .live_iter()
        .find(|s| s.lane.is_sky() && !s.dead(fix) && star_over_cell(s, erased, g))
        .map(|s| (s.pos(fix, false), s.class));
    let at_home = sky
        .live_iter()
        .filter(|s| s.lane.is_sky() && !s.dead(fix) && star_over_cell(s, (5, caret + 1), g))
        .map(|s| s.class)
        .collect();
    let mend = mended.then_some(Mend {
        at: last,
        disp: 0.9,
        row: erased.0,
        col: erased.1,
        deletes: u8::try_from(deletes).expect("a mend counts at most two"),
    });
    hot_event(&mut sky, &ribbon, (g, c), &glyph, fix, (5, caret + 1), mend);
    (sky, fix, erased, retiring, at_home)
}

#[test]
fn a_mend_s_star_is_born_where_the_typo_was_and_dragged_to_the_fix() {
    let mut c = cfg(true);
    c.theme_bg = SHIPPED_GROUND;
    let g = geom_cell(15, 28);
    let window_ms = MEND_PULL_MS as u64;
    let over = |p: (i32, i32), cell: (u16, u16)| {
        px_col(p.0 as f32, g) == i32::from(cell.1) && in_sky_of_row(p.1 as f32, cell.0, g)
    };
    for (label, typo, deletes, fix_after_ms) in [
        ("one-slot fix", TypedClass::Glyph, 1u16, 125u64),
        ("500 ms fix", TypedClass::Glyph, 1, 500),
        ("two deletes", TypedClass::Glyph, 2, 125),
        ("capital typo", TypedClass::Capital, 1, 125),
    ] {
        let t0 = Instant::now();
        let (mut sky, fix, erased, retiring, home_before) =
            mend_sky(typo, deletes, fix_after_ms, true, &c, t0);
        let home = (erased.0, erased.1 + 1);
        if typo == TypedClass::Capital {
            assert!(
                home_before.contains(&StarClass::M1),
                "NON-VACUOUS: the capital typo's earned hero is not over the caret's cell"
            );
        }
        let at = |ms: u64| fix + Duration::from_millis(ms);
        let fix_born: Vec<Star> = sky
            .live_iter()
            .filter(|s| s.born == fix && s.lane.is_sky())
            .copied()
            .collect();
        let typo_at = |p: (i32, i32)| retiring.is_some_and(|(r, _)| r == p) || over(p, erased);
        let Some(star) = fix_born
            .iter()
            .find(|s| typo_at(s.pos(fix, false)) && over(s.pos(at(window_ms), false), home))
        else {
            panic!(
                "{label}: no star of the fix's stands where the typo was ({erased:?}, retiring {retiring:?}) at +0 and over the caret's {home:?} at +{window_ms} ms — the fix bore {:?}",
                fix_born
                    .iter()
                    .map(|s| (s.class, s.pos(fix, false), s.pos(at(window_ms), false)))
                    .collect::<Vec<_>>()
            );
        };
        assert_eq!(
            star.class,
            StarClass::M2,
            "{label}: the mend's star is an m2"
        );

        let own = |ms: u64| {
            let p = star.pos(at(ms), false);
            p.0 + star.stream_px(ms as f32)
        };
        let (x0, y0) = star.pos(fix, false);
        let mut prev = own(0);
        for ms in 1..=window_ms {
            let now = own(ms);
            assert!(
                now >= prev,
                "{label}: the pull went backward at +{ms} ms ({prev} → {now})"
            );
            prev = now;
        }
        let travelled = own(window_ms) - own(0);
        assert_eq!(
            travelled,
            star.pull.round() as i32,
            "{label}: pulled {travelled} px, priced {:.1}",
            star.pull
        );
        for ms in window_ms + 1..=400 {
            assert_eq!(
                own(ms),
                own(window_ms),
                "{label}: the star moved at +{ms} ms, after its window"
            );
        }

        if let Some((p, class)) = retiring {
            assert_eq!(
                x0, p.0,
                "{label}: born at x {x0}, the retiring {class:?} stood at {}",
                p.0
            );
            assert!(
                (y0 - p.1).abs() <= 2,
                "{label}: born at y {y0}, the retiring {class:?} stood at {}",
                p.1
            );
        }
        assert!(
            !sky.live_iter()
                .any(|s| s.born < fix && s.lane.is_sky() && star_over_cell(s, erased, g)),
            "{label}: a star born before the fix is still over the erased cell {erased:?}"
        );
        assert!(
            !fix_born
                .iter()
                .any(|s| s.pull == 0.0 && over(s.pos(fix, false), erased)),
            "{label}: the fix bore a fresh star over the erased cell {erased:?} beside the one it pulled away"
        );
        assert_eq!(
            fix_born
                .iter()
                .filter(|s| over(s.pos(at(window_ms), false), home))
                .count(),
            1,
            "{label}: the fix bore more than one sky star over the caret's cell"
        );
        let at_home: Vec<(StarClass, StarLane, (i32, i32))> = {
            let now = at(window_ms);
            let mut cx = ctx_in(g, now, &c, home);
            cx.disp = 0.9;
            cx.birth_disp = 0.9;
            let mut sc = Scratch::default();
            let mut f = sc.frame();
            sky.emit(&cx, &mut f);
            sky.live_iter()
                .filter(|s| s.lane.is_sky() && over(s.pos(now, false), home))
                .map(|s| (s.class, s.lane, s.pos(now, false)))
                .collect()
        };
        assert_eq!(
            at_home.len(),
            1,
            "{label}: the caret's cell holds {at_home:?} at +{window_ms} ms — one star per cell"
        );

        let moving_ms = window_ms.max(EXHAUST_THROW_MS as u64);
        assert!(sky.brisk(at(55)), "{label}: not brisk mid-pull");
        assert!(
            star.brisk(at(window_ms - 1)),
            "{label}: the mend's star is not brisk one millisecond before its pull ends"
        );
        assert!(
            !star.brisk(at(window_ms + 1)),
            "{label}: the mend's star is still brisk one millisecond after its pull"
        );
        assert!(
            !sky.brisk(at(moving_ms + 1)),
            "{label}: still brisk one millisecond after the last thing the fix set moving"
        );

        for ms in [0u64, 55, window_ms] {
            let now = at(ms);
            let mut cx = ctx_in(g, now, &c, home);
            cx.disp = 0.9;
            cx.birth_disp = 0.9;
            let mut sc = Scratch::default();
            let mut f = sc.frame();
            sky.emit(&cx, &mut f);
            let (x, y) = star.pos(now, false);
            let centre = composite_over(&sc, SHIPPED_GROUND, x, y, 1, 1)[0];
            let lum = relative_luminance(centre) * 255.0;
            assert!(
                lum <= STAR_LIGHT_CEIL * 255.0,
                "{label}: the mend's star centre at +{ms} ms reads Y {lum:.1} over the \
                 caret law's ceiling"
            );
        }
    }
}

use crate::kitty_pet::{PetAction, PetBrain, PetFrame, PetSense};
use crate::rainbow_kitty::companion::{CATCH_REACH_CELLS, PetOffer, PetOnGlass, StarCatch};
use crate::rainbow_kitty::{Dir, Engine, Licence};

const fn paw_geom() -> Geom {
    Geom {
        cw: 10,
        ch: 20,
        rows: 30,
        cols: 100,
        origin_x: 0,
        origin_y: 0,
        win_w: 1000,
        win_h: 600,
        head: 0,
    }
}

const fn paw_sense(now: Instant, caret: (u16, u16)) -> PetSense {
    PetSense {
        caret_drawn: true,
        now,
        caret: Some(caret),
        wrapped: false,
        rows: 30,
        cols: 100,
        cell_w: 10,
        cell_h: 20,
        reduced_motion: false,
        output_burst: false,
        pointer: None,
    }
}

const FRAME_MS: u64 = 16;

const WRAP_COL: u16 = 90;

#[derive(Clone, Copy, Debug)]
struct Step {
    at: Instant,
    pet: PetFrame,
    twin_pet: PetFrame,
    offer: PetOffer,
    caught: Option<(StarCatch, bool)>,
    emitted: (usize, usize, usize),
    twin_emitted: (usize, usize, usize),
}

struct Lockstep {
    eng: Engine,
    pet: PetBrain,
    twin_eng: Engine,
    twin_pet: PetBrain,
    sc: Scratch,
    tsc: Scratch,
    cfg: Config,
    t: Instant,
    caret: (u16, u16),
    last: Option<Step>,
}

impl Lockstep {
    fn new(t0: Instant, caret: (u16, u16)) -> Self {
        let mut eng = Engine::new();
        eng.set_engaged(true);
        let mut twin_eng = Engine::new();
        twin_eng.set_engaged(true);
        Self {
            eng,
            pet: PetBrain::default(),
            twin_eng,
            twin_pet: PetBrain::default(),
            sc: Scratch::default(),
            tsc: Scratch::default(),
            cfg: cfg(true),
            t: t0,
            caret,
            last: None,
        }
    }

    fn frame(&mut self) -> Step {
        let g = paw_geom();
        let t = self.t;
        clear(&mut self.sc);
        clear(&mut self.tsc);
        {
            let mut fr = self.sc.frame();
            self.eng.tick(t, g, &self.cfg, &mut fr);
        }
        {
            let mut fr = self.tsc.frame();
            self.twin_eng.tick(t, g, &self.cfg, &mut fr);
        }
        let pet = self.pet.tick(paw_sense(t, self.caret));
        let twin_pet = self.twin_pet.tick(paw_sense(t, self.caret));
        let offer = self.eng.pet_offer(g, PetOnGlass::of(&pet, g));
        let caught = self
            .pet
            .note_v2_offer(&offer)
            .map(|star| (star, self.eng.catch_star(star, t)));
        let step = Step {
            at: t,
            pet,
            twin_pet,
            offer,
            caught,
            emitted: emitted(&self.sc),
            twin_emitted: emitted(&self.tsc),
        };
        self.last = Some(step);
        step
    }

    fn next_frame(&mut self) -> Step {
        self.t += Duration::from_millis(FRAME_MS);
        self.frame()
    }

    fn advance(&mut self, gap: Duration) {
        let due = self.t + gap;
        while self.t + Duration::from_millis(FRAME_MS) < due {
            self.next_frame();
        }
        self.t = due;
    }

    fn key(&mut self, class: TypedClass) -> Step {
        let (r, c) = self.caret;
        let t = self.t;
        let to = (r, c + 1);
        for e in self.engines() {
            e.probe_mut().probe_row(i32::from(r) - 1, &[false; 100]);
            e.probe_mut().probe_row(i32::from(r), &[false; 100]);
            e.on_event(
                Event::Move {
                    from: (r, c),
                    to,
                    licence: Licence::Typed,
                    dir: Dir::of(1, 0),
                },
                t,
            );
            e.on_event(
                Event::Typed {
                    cells: 1,
                    shifted: matches!(class, TypedClass::Capital),
                    class,
                },
                t,
            );
        }
        self.caret = to;
        self.frame()
    }

    fn backspace(&mut self) -> Step {
        let (r, c) = self.caret;
        let t = self.t;
        let to = (r, c.saturating_sub(1));
        for e in self.engines() {
            e.on_event(
                Event::Move {
                    from: (r, c),
                    to,
                    licence: Licence::Typed,
                    dir: Dir::of(-1, 0),
                },
                t,
            );
            e.on_event(Event::Erase, t);
        }
        self.caret = to;
        self.frame()
    }

    fn newline(&mut self) -> Step {
        let (r, c) = self.caret;
        let t = self.t;
        let to = (r + 1, 0);
        for e in self.engines() {
            e.on_event(
                Event::Move {
                    from: (r, c),
                    to,
                    licence: Licence::Return,
                    dir: Dir::of(-i32::from(c), 1),
                },
                t,
            );
            e.on_event(Event::Return, t);
        }
        self.caret = to;
        self.frame()
    }

    fn key_after(&mut self, gap: Duration, class: TypedClass) -> Step {
        self.advance(gap);
        if self.caret.1 >= WRAP_COL {
            self.newline();
            self.advance(gap);
        }
        self.key(class)
    }

    fn engines(&mut self) -> impl Iterator<Item = &mut Engine> {
        [&mut self.eng, &mut self.twin_eng].into_iter()
    }

    fn cat(&self) -> Option<PetOnGlass> {
        self.last.and_then(|s| PetOnGlass::of(&s.pet, paw_geom()))
    }

    fn star(&self, star: StarCatch) -> Option<Star> {
        find_star(&self.eng, star)
    }

    fn twin_star(&self, star: StarCatch) -> Option<Star> {
        find_star(&self.twin_eng, star)
    }
}

fn clear(sc: &mut Scratch) {
    sc.under.clear();
    sc.out.clear();
    sc.halos.clear();
    sc.beams.clear();
    sc.cues.clear();
}

const fn emitted(sc: &Scratch) -> (usize, usize, usize) {
    (sc.under.len() + sc.out.len(), sc.halos.len(), sc.cues.len())
}

fn find_star(eng: &Engine, star: StarCatch) -> Option<Star> {
    eng.stardust
        .live_iter()
        .find(|s| s.born == star.born && s.x == star.x && s.y == star.y)
        .copied()
}

fn gold_born(eng: &Engine, at: Instant) -> Option<Star> {
    eng.stardust
        .live_iter()
        .find(|s| s.gold && s.class == StarClass::M1 && s.lane.is_sky() && s.born == at)
        .copied()
}

fn gold_capital_at(eng: &Engine, cell: (u16, u16)) -> Option<(u16, u16)> {
    let free = eng.stardust.free_sky_cell(cell, paw_geom())?;
    let seed = cell_hash(free.0, free.1, eng.stardust.minted.wrapping_add(1));
    deal_tint(seed, 0.0).1.then_some(free)
}

fn rally_until_seated(w: &mut Lockstep, gap: Duration, rounds: u32) -> u32 {
    for round in 0..rounds {
        for _ in 0..3 {
            w.key_after(gap, TypedClass::Glyph);
        }
        for _ in 0..2 {
            w.advance(gap);
            w.backspace();
        }
        if w.cat().is_some_and(|c| {
            c.contented() && w.last.is_some_and(|s| s.pet.action == PetAction::Sit)
        }) {
            return round + 1;
        }
    }
    panic!("fixture: the rally did not seat the cat in {rounds} rounds");
}

#[test]
fn a_gold_star_walks_from_the_deal_through_the_paw_to_the_catch() {
    let t0 = Instant::now();
    let g = paw_geom();
    let gap = Duration::from_micros(83_333);
    let mut w = Lockstep::new(t0, (4, 0));

    for _ in 0..30 {
        w.key_after(gap, TypedClass::Glyph);
    }
    let rounds = rally_until_seated(&mut w, gap, 12);
    let seat = w.cat().expect("fixture: the cat is on glass");
    assert!(seat.contented(), "fixture: a contented seat");

    let mut dealt: Option<(Step, (u16, u16))> = None;
    'rounds: for _ in 0..rounds + 40 {
        for k in 0..3 {
            w.advance(gap);
            let cell = w.caret;
            let cat = w.cat().expect("fixture: the cat stays on glass");
            let in_reach =
                cat.contented() && cat.columns_to(i32::from(cell.1)) <= CATCH_REACH_CELLS;
            if k == 2
                && in_reach
                && let Some(free) = gold_capital_at(&w.eng, cell)
            {
                let s = w.key(TypedClass::Capital);
                dealt = Some((s, free));
                break 'rounds;
            }
            w.key(TypedClass::Glyph);
        }
        for _ in 0..2 {
            w.advance(gap);
            w.backspace();
        }
    }
    let (birth, free) = dealt.expect("fixture: the seed knob must read gold within 40 rounds");

    let star = gold_born(&w.eng, birth.at).expect("the capital dealt a gold m1");
    assert_eq!(
        star.grid_col(g),
        i32::from(free.1),
        "born on the knob's cell"
    );
    assert!(star.in_sky_of(4, g), "born in the sky of the cat's row");
    assert_eq!(
        gold_born(&w.twin_eng, birth.at).map(|s| (s.x, s.y, s.born)),
        Some((star.x, star.y, star.born)),
        "the twin dealt the same star: the seam mints nothing"
    );
    assert!(
        star.finish.is_none(),
        "born with its whole life ahead of it"
    );

    let catch = StarCatch {
        x: star.x,
        y: star.y,
        born: star.born,
        col: star.grid_col(g),
    };
    assert!(
        birth.pet.action.settled() && birth.pet.purr > 0.0,
        "a settled, purring cat"
    );
    assert_eq!(
        birth.offer.catch,
        Some(catch),
        "the birth frame's offer names the star"
    );
    assert_eq!(birth.caught, None, "noting is not catching");
    assert!(
        w.pet.pending_v2_offer().1,
        "the star is latched in the brain"
    );

    let mut contact = None;
    for _ in 0..20 {
        let before = w.star(catch).expect("the star lives until the paw");
        assert_eq!(before.finish, None, "the life moved before the paw landed");
        let s = w.next_frame();
        if let Some((back, hit)) = s.caught {
            assert_eq!(back, catch, "the star handed back is the star offered");
            assert!(hit, "the sky still had the star to catch");
            contact = Some(s);
            break;
        }
    }
    let contact = contact.expect("the paw lands inside the star's TTL");
    let horizon = contact.at + Duration::from_secs_f32(CATCH_FINISH_MS / 1000.0);

    let caught = w
        .star(catch)
        .expect("the caught star is still on glass on the paw's frame");
    assert_eq!(
        caught.finish,
        Some(Finish {
            at: contact.at,
            span_s: CATCH_FINISH_MS / 1000.0,
        }),
        "the catch spends the star on the sky's own finish"
    );
    assert_eq!(
        w.twin_star(catch).map(|s| s.finish),
        Some(None),
        "the un-offered twin's copy keeps its whole life"
    );
    let others = |e: &Engine| -> Vec<(u32, u32, Instant, Option<Finish>)> {
        let mut v: Vec<_> = e
            .stardust
            .live_iter()
            .filter(|s| !(s.born == catch.born && s.x == catch.x && s.y == catch.y))
            .map(|s| (s.x.to_bits(), s.y.to_bits(), s.born, s.finish))
            .collect();
        v.sort_by(|a, b| a.2.cmp(&b.2).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));
        v
    };
    assert_eq!(
        others(&w.eng),
        others(&w.twin_eng),
        "a bystander's life moved"
    );
    assert!(
        !others(&w.eng).is_empty(),
        "fixture: there were bystanders to protect"
    );

    assert_eq!(
        contact.pet.motes.iter().flatten().count(),
        contact.twin_pet.motes.iter().flatten().count(),
        "the reach spawned a mote"
    );
    assert_eq!(
        contact.emitted, contact.twin_emitted,
        "the catch drew or sounded something"
    );
    assert_eq!(
        w.eng.fingerprint(),
        w.twin_eng.fingerprint(),
        "the contact frame's light is byte-identical to the un-offered world's"
    );

    assert_eq!(
        (
            contact.pet.col.to_bits(),
            contact.pet.row.to_bits(),
            contact.pet.lift.to_bits()
        ),
        (
            contact.twin_pet.col.to_bits(),
            contact.twin_pet.row.to_bits(),
            contact.twin_pet.lift.to_bits()
        ),
        "the offered cat is at ({}, {}) and the un-offered twin at ({}, {})",
        contact.pet.col,
        contact.pet.row,
        contact.twin_pet.col,
        contact.twin_pet.row
    );

    let mut catches = 1;
    while w.t < horizon {
        let s = w.next_frame();
        catches += usize::from(s.caught.is_some());
        if let Some(s) = w.star(catch) {
            assert_eq!(
                s.finish.map(|f| f.at),
                Some(contact.at),
                "the finish was re-stamped after the paw"
            );
        }
    }
    let s = w.next_frame();
    catches += usize::from(s.caught.is_some());
    assert!(w.t >= horizon);
    assert!(
        w.star(catch).is_none(),
        "the caught star is still in the sky past the horizon"
    );
    assert!(
        w.twin_star(catch).is_some_and(|s| !s.dead(w.t)),
        "fixture: the un-offered twin's star lives on past the horizon (m1 sky life 460 ms)"
    );

    for _ in 0..90 {
        let s = w.next_frame();
        catches += usize::from(s.caught.is_some());
    }
    assert_eq!(catches, 1, "the paw handed a star back {catches} times");

    let mut t_zero = None;
    for _ in 0..(20 * 1000 / FRAME_MS) {
        w.next_frame();
        if w.eng.fingerprint() == 0
            && !w.eng.needs_frame_cadence()
            && w.eng.next_change_deadline(w.t).is_none()
        {
            t_zero = Some(w.t);
            break;
        }
    }
    assert!(
        t_zero.is_some(),
        "the sky idles to exactly zero within 20 s of the catch"
    );
    assert_eq!(w.eng.status().stars, 0);
    assert_eq!(
        w.eng.pet_offer(g, w.cat()),
        PetOffer::default(),
        "an idle sky offers the resident nothing"
    );
    assert_eq!(
        w.pet.pending_v2_offer(),
        (false, false),
        "a latch outlived the catch"
    );
    assert_eq!(w.eng.fingerprint(), 0, "the sky lit again while idle");
}

fn steady_sky(gm: Geom, cps: f32, keys: u16) -> (Stardust, Scratch) {
    let c = cfg(true);
    let t0 = Instant::now();
    let ribbon = Ribbon::new();
    let mut sky = Stardust::new();
    sky.probe_mut().probe_row(4, &[false; 120]);
    let mut sc = Scratch::default();
    for k in 0..keys {
        let at = t0 + Duration::from_micros((f32::from(k) * 1e6 / cps) as u64);
        let mut cx = ctx_in(gm, at, &c, (5, 10 + k));
        cx.disp = disp_at_12cps(k).max(0.9);
        cx.birth_disp = cx.disp;
        sky.budget.refill(at);
        sky.on_event(&typed(TypedClass::Glyph), at, &cx, &ribbon);
        sc = Scratch::default();
        sky.emit(&cx, &mut sc.frame());
    }
    (sky, sc)
}

#[test]
fn a_point_query_reads_one_cell_not_the_whole_sky() {
    for (label, gm, cps) in [
        ("12 cps 1x", geom(), 12.0f32),
        ("125 cps retina", geom_cell(15, 28), 125.0),
    ] {
        GROUND_WORK.with(|w| w.set([0; 4]));
        steady_sky(gm, cps, 48);
        let w = GROUND_WORK.with(std::cell::Cell::get);
        let (scan, cell) = (w[0] + w[1], w[2] + w[3]);
        assert!(scan > 0, "{label}: the sky never priced a star");
        assert!(
            cell * 10 <= scan,
            "{label}: the indexed ground still reads {cell} entries against the scan's \
             {scan} — the cell index is not narrowing the query"
        );
    }
}

#[test]
fn the_slot_hash_is_injective_over_every_box_the_cap_allows() {
    fn deltas(cap: i32) -> Vec<(i32, i32)> {
        let mut v: Vec<(i32, i32)> = Vec::new();
        for w in 1..=cap {
            for h in 1..=cap {
                if w * h > cap {
                    continue;
                }
                for dx in -(w - 1)..w {
                    for dy in -(h - 1)..h {
                        if (dx, dy) != (0, 0) && !v.contains(&(dx, dy)) {
                            v.push((dx, dy));
                        }
                    }
                }
            }
        }
        v
    }

    let cap = i32::try_from(SKY_INDEX_WIDE_CELLS).expect("the cap is small");
    let offsets = deltas(cap);
    assert_eq!(
        offsets.len(),
        136,
        "a 16-cell box reaches 136 offsets; if this moved, the cap moved \
         and the sweep below is no longer the sweep this law needs"
    );
    for cx in 0..1_i32 << 13 {
        let base = SkyIndex::slot((cx, 0));
        for &(dx, dy) in &offsets {
            assert_ne!(
                base,
                SkyIndex::slot((cx + dx, dy)),
                "cells (0, 0) and ({dx}, {dy}) of ONE box share a chain at \
                 base cell ({cx}, 0) — a lay filed under both would have \
                 its light added twice by `sky_ground`"
            );
        }
    }

    let doubled = (0..1_i32 << 13)
        .filter(|&cx| SkyIndex::slot((cx, 0)) == SkyIndex::slot((cx, 19)))
        .count();
    assert!(
        doubled > 0,
        "the `(0, 19)` offset collided at none of 8192 base cells, so \
         this arm no longer shows the cap is what makes the law true"
    );
}

#[test]
fn a_lay_too_wide_to_file_spills_the_index_and_still_answers() {
    let gm = geom();
    let mut sc = Scratch::default();
    sc.out.push(GlowQuad {
        row: 5,
        x: 90,
        y: 130,
        w: 360,
        h: 4,
        color: 0x0010_2030,
        alpha: 0,
        color2: 0x0010_2030,
        alpha2: 0,
    });
    let mut index = SkyIndex::default();
    index.open(gm, (0, 0));
    {
        let frame = sc.frame();
        index.ingest(&frame);
    }
    assert!(index.spilled, "a 40-cell lay was filed as if it were one");
    let frame = sc.frame();
    for (x, y) in [(90, 130), (300, 131), (449, 133), (89, 130), (450, 130)] {
        assert_eq!(
            index.ground(0, &frame, x, y),
            sky_ground(0, &*frame.out, &*frame.halos, x, y),
            "({x}, {y}) under a spilled index"
        );
    }
    index.open(gm, (0, 0));
    assert!(!index.spilled, "the spill outlived its frame");
}

#[test]
fn a_cell_less_geometry_falls_back_to_one_chain() {
    let gm = Geom {
        cw: 0,
        ch: 0,
        ..geom()
    };
    let mut sc = Scratch::default();
    sc.out.push(GlowQuad {
        row: 0,
        x: 90,
        y: 100,
        w: 4,
        h: 2,
        color: 0x0020_3040,
        alpha: 0,
        color2: 0x0020_3040,
        alpha2: 0,
    });
    let mut index = SkyIndex::default();
    index.open(gm, (0, 0));
    {
        let frame = sc.frame();
        index.ingest(&frame);
    }
    let frame = sc.frame();
    for (x, y) in [
        (90, 100),
        (93, 101),
        (89, 100),
        (94, 100),
        (90, 99),
        (90, 102),
    ] {
        assert_eq!(
            index.ground(0, &frame, x, y),
            sky_ground(0, &*frame.out, &*frame.halos, x, y),
            "({x}, {y}) under a cell-less geometry"
        );
    }
}

/// A star on a keep-out box's edge (dark and paper) or the paper caret's margin makes at most
/// [`crate::TWINKLE_FLASH_BOUND_HZ`] visibility transitions a second and keeps one mark reach.
/// The negative control culls by the drawn arm and toggles past the bound.
#[test]
fn a_star_on_a_cull_s_edge_never_blinks_with_its_arm() {
    let t0 = Instant::now();
    let g = geom();
    let bound = crate::TWINKLE_FLASH_BOUND_HZ as usize;
    let at = |k: u64| t0 + Duration::from_micros(k * 16_667);
    let worst_second = |on: &[bool]| {
        on.windows(61)
            .map(|w| w.windows(2).filter(|p| p[0] != p[1]).count())
            .max()
            .unwrap_or(0)
    };
    let caret_far = (3u16, 10u16);
    let caret = (3u16, 30u16);
    let margin = i32::from(g.origin_x) + i32::from(caret.1) * g.cw as i32 - g.cw as i32;
    let mut control_worst = 0usize;
    for dark in [true, false] {
        let c = cfg(dark);
        for class in [StarClass::M1, StarClass::M2, StarClass::M3] {
            for lane in [StarLane::Strike, StarLane::Field] {
                let lo = arm_trough_px(class, g.ch as f32) + 1;
                let hi = arm_peak_px(class, g.ch as f32) + 1;
                // (star x, keep-out box, caret) for each placement.
                let mut placements = Vec::new();
                let x = 200i32;
                for x0 in (x + lo / 2 - 1)..=(x + hi / 2 + 1) {
                    placements.push((x, Some((x0, x0 + 30, 96, 104)), caret_far));
                }
                if !dark {
                    for sx in (margin - hi - 1)..=(margin - lo + 1) {
                        placements.push((sx, None, caret));
                    }
                }
                let (mut drew, mut culled) = (false, false);
                for (sx, boxed, car) in placements {
                    let mut st = star(class, lane, t0, TINT_WHITE_RGB);
                    st.x = sx as f32;
                    let mut sky = sky();
                    sky.sow(st);
                    if let Some(b) = boxed {
                        sky.keep_out.set([b]);
                    }
                    let (mut on, mut old, mut reaches) = (Vec::new(), Vec::new(), Vec::new());
                    for k in 0..240u64 {
                        let cx = ctx_in(g, at(k), &c, car);
                        let mut sc = Scratch::default();
                        sky.emit(&cx, &mut sc.frame());
                        on.push(sc.out.len() + sc.halos.len() > 0);
                        reaches.extend(sky.marks().first().map(|m| m.reach));
                        // The control: the same cull keyed on the drawn arm.
                        let painted = Paint::of(&st, &sky.probe, &sky.shade, &cx);
                        old.push(
                            st.inks(&c)
                                && painted.is_some_and(|p| {
                                    let reach = p.arm.max(p.arm_lo) + 1;
                                    !(sky.keep_out.covers((p.x, p.y), reach / 2)
                                        || cx.near_paper_caret((p.x, p.y), reach))
                                }),
                        );
                    }
                    let what = format!(
                        "dark={dark} {class:?}/{lane:?} star x {sx} box {boxed:?} caret {car:?}"
                    );
                    let n = worst_second(&on);
                    assert!(
                        n <= bound,
                        "{what}: {n} visibility transitions in one second (bound {bound})"
                    );
                    reaches.dedup();
                    assert!(
                        reaches.len() <= 1,
                        "{what}: the star's mark changes reach {reaches:?}"
                    );
                    drew |= on.iter().any(|&o| o);
                    culled |= !on.iter().any(|&o| o);
                    control_worst = control_worst.max(worst_second(&old));
                }
                assert!(
                    drew && culled,
                    "dark={dark} {class:?}/{lane:?}: the sweep never crosses the cull's edge (drew {drew}, culled {culled})"
                );
            }
        }
    }
    assert!(
        control_worst > bound,
        "control: culled by the drawn arm, the worst placement toggles only {control_worst} times in a second"
    );
}
