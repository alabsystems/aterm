// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Digital rain tests for the CPU renderer: the rain's cells as one Linear
//! Under fx layer a grid row (`aterm_effects::matrix_rain::RainLayers`), and
//! its additive halos.

use std::collections::BTreeMap;
use std::sync::Arc;

use aterm_core::render::{
    FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxSlot, FxSpace, FxSprite, PxRect, RainHalo,
    RenderInput,
};
use aterm_core::terminal::Terminal;
use aterm_effects::matrix_rain::{RainBaker, RainCell, RainLayers, rom};
use aterm_render::{
    DirtyDecision, Frame, Renderer, Theme, WindowCpu, compute_dirty_rows, rgb_to_u32,
};

use crate::sprites::{art_of, patterned, solid, under};

fn renderer() -> Renderer {
    Renderer::from_system(18.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature")
}

fn cell_pixels(f: &Frame, cw: usize, ch: usize, row: usize, col: usize) -> Vec<u32> {
    let mut out = Vec::with_capacity(cw * ch);
    for y in row * ch..(row * ch + ch).min(f.height) {
        for x in col * cw..(col * cw + cw).min(f.width) {
            out.push(f.pixels[y * f.width + x]);
        }
    }
    out
}

/// One layer of the rain's kind (Linear, Under, no knock-out) drawing `art`
/// 1:1 at `(x, y)`, tinted and faded.
fn rain_layer(x: i32, y: i32, art: Arc<FxArt>, tint: u32, alpha: u8, flip_x: bool) -> FxLayer {
    let (w, h) = (art.w(), art.h());
    rain_layer_sized(x, y, w, h, art, tint, alpha, flip_x)
}

/// [`rain_layer`] with `art` stretched to `w × h`.
#[allow(clippy::too_many_arguments)]
fn rain_layer_sized(
    x: i32,
    y: i32,
    w: u16,
    h: u16,
    art: Arc<FxArt>,
    tint: u32,
    alpha: u8,
    flip_x: bool,
) -> FxLayer {
    FxLayer::new(
        1,
        Arc::new(FxContent::new(
            vec![FxItem::Sprite(FxSprite {
                x,
                y,
                dw: w,
                dh: h,
                flip_x,
                art,
                ink: 0,
                clip: PxRect::EVERYWHERE,
            })],
            vec![FxInk::over(tint, alpha)],
        )),
        FxSlot::Under,
        FxSpace::Grid,
        FxBlend::Linear,
    )
}

/// A tile set baked whole at `cw × ch`.
fn baker(cw: u16, ch: u16) -> RainBaker {
    let master = rom::rasterize_master();
    let mut b = RainBaker::default();
    b.begin_frame(cw, ch);
    while !b.complete() {
        b.bake_tiles(&master);
    }
    b
}

/// Glyph `glyph` in cell `(row, col)` of a `cw × ch` grid.
fn rain_cell(row: u16, col: u16, glyph: u8, (cw, ch): (u16, u16)) -> RainCell {
    RainCell {
        row,
        x: col * cw,
        y: row * ch,
        w: cw,
        h: ch,
        glyph,
        tint: 0x0040_E070,
        alpha: 200,
        flip_x: false,
    }
}

fn halo(row: u16, x: u16, y: u16, w: u16, h: u16, color: u32) -> RainHalo {
    RainHalo {
        row,
        x,
        y,
        w,
        h,
        color,
        cx: x + w / 2,
        cy: y + h / 2,
        rx: (w / 2).max(1),
        ry: (h / 2).max(1),
        ..Default::default()
    }
}

fn mul8(c: u32, f: u32) -> u32 {
    (c * f + 127) / 255
}

fn sat_add(dst: u32, premul: u32) -> u32 {
    let ch = |sh: u32| (((dst >> sh) & 0xff) + ((premul >> sh) & 0xff)).min(255);
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

#[test]
fn rain_stamp_mul8_tint_alpha_flip_and_band_containment() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(4, 12);
    term.process(b"\x1b[?25l");
    let base = rend.render_input(&term.cell_frame(4, 12)).pixels.clone();

    let mut input = term.cell_frame(4, 12);
    let grey = solid([200, 200, 200, 255], cw as u16, ch as u16);
    input.fx_layers = vec![rain_layer(0, ch as i32, grey, 0x0080_40C0, 255, false)];
    let f = rend.render_input(&input);
    let want = (mul8(200, 0x80) << 16) | (mul8(200, 0x40) << 8) | mul8(200, 0xC0);
    for dy in 0..ch {
        for dx in 0..cw {
            assert_eq!(
                f.pixels[(ch + dy) * f.width + dx],
                want,
                "tinted opaque stamp must be the exact mul8 product at ({dx},{dy})"
            );
        }
    }
    for (i, (&b, &p)) in base.iter().zip(f.pixels.iter()).enumerate() {
        if b != p {
            let y = i / f.width;
            assert!(
                (ch..2 * ch).contains(&y),
                "rain stamp must stay inside its cell band: pixel row {y} changed"
            );
        }
    }

    let faint = |a: u8| solid([255, 255, 255, a], cw as u16, ch as u16);
    let mut input = term.cell_frame(4, 12);
    input.fx_layers = vec![rain_layer(0, ch as i32, faint(127), 0x00FF_FFFF, 1, false)];
    assert_eq!(
        base,
        rend.render_input(&input).pixels,
        "mul8(127, 1) == 0: a fully-rounded-away alpha must paint nothing"
    );
    let mut input = term.cell_frame(4, 12);
    input.fx_layers = vec![rain_layer(0, ch as i32, faint(128), 0x00FF_FFFF, 1, false)];
    assert_ne!(
        base,
        rend.render_input(&input).pixels,
        "mul8(128, 1) == 1: one unit of coverage must survive the rounding"
    );

    let (aw, ah) = (cw.min(64) as u16, ch.min(64) as u16);
    let art = patterned(aw, ah);
    let mut input = term.cell_frame(4, 12);
    input.fx_layers = vec![rain_layer(
        3,
        2 * ch as i32,
        art.clone(),
        0x00FF_FFFF,
        255,
        true,
    )];
    let f = rend.render_input(&input);
    let d = art.data();
    for dy in 0..usize::from(ah) {
        for dx in 0..usize::from(aw) {
            let i = (dy * usize::from(aw) + usize::from(aw) - 1 - dx) * 4;
            let want = (u32::from(d[i]) << 16) | (u32::from(d[i + 1]) << 8) | u32::from(d[i + 2]);
            assert_eq!(
                f.pixels[(2 * ch + dy) * f.width + 3 + dx],
                want,
                "flip_x must mirror the source column at ({dx},{dy})"
            );
        }
    }
}

#[test]
fn rain_under_text_and_under_the_companions() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(2, 8);
    term.process("\x1b[?25l█".as_bytes());

    let rain_green = 0x0020_C840;
    let cat_red = 0x00C8_2020;
    let mut input = term.cell_frame(2, 8);
    let rain = art_of(8, 8, |_, _| [0x20, 0xC8, 0x40, 255]);
    input.fx_layers.push(rain_layer_sized(
        0,
        0,
        (3 * cw) as u16,
        ch as u16,
        rain,
        0x00FF_FFFF,
        255,
        false,
    ));
    crate::sprites::put(
        &mut input,
        &[aterm_render::sprites::FreeSprite {
            w: cw as u16,
            h: ch as u16,
            ..under((2 * cw) as i32, 0, solid([0xC8, 0x20, 0x20, 255], 8, 8))
        }],
    );

    let f = rend.render_input(&input);
    let fg = rgb_to_u32(input.cells[0][0].fg);
    assert!(
        cell_pixels(&f, cw, ch, 0, 0).iter().all(|&p| p == fg),
        "the full-block glyph must draw OVER the rain (cell 0 pure fg)"
    );
    assert!(
        cell_pixels(&f, cw, ch, 0, 1)
            .iter()
            .all(|&p| p == rain_green),
        "the uncovered rain must be the exact rain colour (cell 1 pure green)"
    );
    assert!(
        cell_pixels(&f, cw, ch, 0, 2).iter().all(|&p| p == cat_red),
        "the companion must draw OVER the rain (cell 2 pure red: cats walk on rain)"
    );
}

#[test]
fn rain_add_is_saturating_add_and_only_brightens() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(3, 10);
    term.process(b"\x1b[?25lglow");
    let base = rend.render_input(&term.cell_frame(3, 10)).pixels.clone();

    let dim = 0x0018_3010;
    let hot = 0x00F0_F0F0;
    let mut input = term.cell_frame(3, 10);
    input.rain_add = vec![
        halo(0, 2, 1, (3 * cw) as u16, (ch - 2) as u16, dim),
        halo(1, 0, ch as u16, (2 * cw) as u16, ch as u16, hot),
    ];
    let f = rend.render_input(&input);

    let pad = (f.width - 10 * cw) / 2;
    let mut expected = base.clone();
    for q in &input.rain_add {
        let x0 = pad + q.x as usize;
        let y0 = pad + q.y as usize;
        let cx = (pad + q.cx as usize) as i32;
        let cy = (pad + q.cy as usize) as i32;
        let rx2 = i32::from(q.rx) * i32::from(q.rx);
        let ry2 = i32::from(q.ry) * i32::from(q.ry);
        for y in y0..(y0 + q.h as usize).min(f.height) {
            let dy = y as i32 - cy;
            let ny = dy * dy * 256 / ry2;
            for x in x0..(x0 + q.w as usize).min(f.width) {
                let dx = x as i32 - cx;
                let nsq = dx * dx * 256 / rx2 + ny;
                let weight = (256 - nsq).max(0);
                let weight = (weight * weight / 256).min(255) as u32;
                let premul = (mul8((q.color >> 16) & 0xff, weight) << 16)
                    | (mul8((q.color >> 8) & 0xff, weight) << 8)
                    | mul8(q.color & 0xff, weight);
                let i = y * f.width + x;
                expected[i] = sat_add(expected[i], premul);
            }
        }
    }
    assert_eq!(
        f.pixels, expected,
        "rain_add must be byte-exact radial saturating add over the base frame"
    );
    for (&b, &p) in base.iter().zip(f.pixels.iter()) {
        for sh in [16, 8, 0] {
            assert!(
                (p >> sh) & 0xff >= (b >> sh) & 0xff,
                "additive light must only brighten"
            );
        }
    }
}

#[test]
fn damaged_path_no_ghosting_as_rain_animates() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let rows = 6usize;
    let mut term = Terminal::new(rows as u16, 12);
    term.process(b"\x1b[?25l");
    let px = (cw as u16, ch as u16);
    let master = rom::rasterize_master();
    let mut tiles = baker(px.0, px.1);
    let mut rain = RainLayers::default();
    let mut make =
        |term: &mut Terminal, tiles: &RainBaker, cells: &[RainCell], add: Vec<RainHalo>| {
            let mut input = term.cell_frame(rows, 12);
            rain.publish(cells, tiles, &mut input.fx_layers);
            input.rain_add = add;
            input
        };
    let (cw16, ch16) = px;
    let in_a = make(
        &mut term,
        &tiles,
        &[
            rain_cell(1, 0, 0, px),
            rain_cell(1, 3, 1, px),
            rain_cell(2, 3, 2, px),
        ],
        vec![halo(2, 3 * cw16, 2 * ch16, cw16, ch16, 0x0030_6030)],
    );
    let in_b = make(
        &mut term,
        &tiles,
        &[
            rain_cell(2, 0, 0, px),
            rain_cell(2, 3, 2, px),
            rain_cell(3, 3, 4, px),
        ],
        vec![halo(3, 3 * cw16, 3 * ch16, cw16, ch16, 0x0030_6030)],
    );
    let column: Vec<RainCell> = (0..rows as u16)
        .map(|r| rain_cell(r, 1, (16 + r) as u8, px))
        .collect();
    let in_c = make(
        &mut term,
        &tiles,
        &column,
        vec![halo(5, cw16, 5 * ch16, cw16, ch16, 0x0018_4018)],
    );
    assert!(tiles.can_rebake(), "fixture: a whole tile set can rebake");
    assert!(tiles.rebake_tiles(&master, u64::MAX));
    let in_d = make(&mut term, &tiles, &column, in_c.rain_add.clone());
    assert!(
        in_d.fx_layers
            .iter()
            .zip(&in_c.fx_layers)
            .all(|(d, c)| !Arc::ptr_eq(&d.content, &c.content)),
        "fixture: a rebake rebuilds every row"
    );

    let mut wc = WindowCpu::new();
    for (name, input) in [("A", &in_a), ("B", &in_b), ("C", &in_c), ("D", &in_d)] {
        let cached = rend.render_input_cached(&mut wc, input).pixels().to_vec();
        let fresh = rend.render_input(input).pixels.clone();
        assert_eq!(
            cached, fresh,
            "cached-damaged frame {name} must equal a fresh full render \
             (no ghost at vacated rows, no missing stamp at new rows)"
        );
    }
}

fn marked(prev: &RenderInput, cur: &RenderInput) -> (aterm_render::DirtyRows, Vec<usize>) {
    let mut dirty = Vec::new();
    let DirtyDecision::Rows(d) = compute_dirty_rows(
        prev,
        cur,
        false,
        None,
        false,
        None,
        aterm_render::RowBands::new(0, 16),
        &mut dirty,
    ) else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    let rows = dirty
        .iter()
        .enumerate()
        .filter_map(|(r, &b)| b.then_some(r))
        .collect();
    (d, rows)
}

#[test]
fn rain_dirty_gate_and_per_row_marking() {
    let mut term = Terminal::new(6, 8);
    term.process(b"\x1b[?25l"); // hidden cursor: no cursor rows in the dirty set
    let px = (8u16, 16u16);
    let master = rom::rasterize_master();
    let mut tiles = baker(px.0, px.1);
    let settled = vec![
        rain_cell(1, 0, 0, px),
        rain_cell(2, 0, 0, px),
        rain_cell(2, 3, 1, px),
        rain_cell(3, 1, 0, px),
    ];
    let halos = vec![halo(4, 0, 64, 8, 16, 0x0010_2010)];
    let mut rain = RainLayers::default();
    let mut frame = |tiles: &RainBaker, cells: &[RainCell], add: &[RainHalo]| {
        let mut input = term.cell_frame(6, 8);
        rain.publish(cells, tiles, &mut input.fx_layers);
        input.rain_add = add.to_vec();
        input
    };

    let prev = frame(&tiles, &settled, &halos);
    let cur = frame(&tiles, &settled, &halos);
    let (d, rows) = marked(&prev, &cur);
    assert!(!d.fx_changed, "settled rain must not set fx_changed");
    assert!(
        !d.rain_add_changed,
        "settled halos must not set rain_add_changed"
    );
    assert!(
        d.is_gate_hit(),
        "settled rain must gate-hit: steady state is free"
    );
    assert!(rows.is_empty(), "settled rain must mark no rows");

    let moved = vec![settled[0], settled[1], settled[3], rain_cell(4, 1, 1, px)];
    let prev = cur;
    let cur = frame(&tiles, &moved, &halos);
    let (d, rows) = marked(&prev, &cur);
    assert!(d.fx_changed && !d.is_gate_hit(), "moved cells must repaint");
    assert_eq!(
        rows,
        vec![2, 4],
        "one layer a row: exactly the rows whose cells changed (the settled rows \
         1 and 3 stay clean)"
    );

    let prev = frame(&tiles, &settled, &halos);
    let mut mutated = settled.clone();
    mutated[3].glyph = 1;
    let cur = frame(&tiles, &mutated, &halos);
    assert_eq!(
        marked(&prev, &cur).1,
        vec![3],
        "a within-row glyph change must mark exactly that row"
    );

    let prev = frame(&tiles, &settled, &halos);
    assert!(tiles.rebake_tiles(&master, u64::MAX));
    let cur = frame(&tiles, &settled, &halos);
    let (d, rows) = marked(&prev, &cur);
    assert!(d.fx_changed && !d.is_gate_hit(), "a rebake must repaint");
    assert_eq!(
        rows,
        vec![1, 2, 3],
        "the rebake must mark every (unmoved) rain row"
    );

    let prev = frame(&tiles, &settled, &halos);
    let cur = frame(&tiles, &settled, &[halo(5, 0, 80, 8, 16, 0x0010_2010)]);
    let (d, rows) = marked(&prev, &cur);
    assert!(!d.fx_changed, "settled cells must not set fx_changed");
    assert!(
        d.rain_add_changed && !d.is_gate_hit(),
        "a moved halo must repaint"
    );
    assert_eq!(
        rows,
        vec![4, 5],
        "a moved halo must mark exactly its prev∪cur rows"
    );
}

fn rows_of(cells: &[RainCell]) -> BTreeMap<u16, Vec<RainCell>> {
    let mut rows = BTreeMap::<u16, Vec<RainCell>>::new();
    for c in cells {
        rows.entry(c.row).or_default().push(*c);
    }
    rows
}

#[test]
fn rain_marking_conforms_to_the_rain_band_containment_model() {
    const COLS: usize = 4;
    let model = aterm_models_effects::rain_band_containment_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let viewport = model
        .consts
        .iter()
        .find(|(name, _)| *name == "Viewport")
        .map(|&(_, rows)| rows)
        .expect("the model names its viewport");
    let rows = viewport as usize;
    let pick = |state: &aterm_spec::interp::State, action: &str, var: &str, value: i64| {
        aterm_spec::interp::pick(&model, state, action, var, value)
    };
    let mut term = Terminal::new(rows as u16, COLS as u16);
    term.process(b"\x1b[?25l"); // hidden cursor: no cursor rows in the dirty set
    let tiles = baker(8, 16);
    // The same tiles under the next version (`vb`): a re-bake of one glyph.
    let mut rebaked = baker(8, 16);
    assert!(rebaked.rebake_tiles(&rom::rasterize_master(), 1));
    assert_ne!(rebaked.version(), tiles.version());
    let mut rain = RainLayers::default();
    let glyph = |row: i64, window: i64| ((row * 3 + window * 5) % 8) as u8;
    let column = |lit: std::ops::RangeInclusive<i64>, window: i64| -> Vec<RainCell> {
        lit.filter(|&r| r < viewport)
            .map(|r| rain_cell(r as u16, 1, glyph(r, window), (8, 16)))
            .collect()
    };
    let (mut marked_rows, mut separating, mut rebuilt) = (0usize, 0usize, 0usize);
    for head in 2..=viewport + 2 {
        let headed = pick(&model.init_state(), "PickHead", "head", head)
            .unwrap_or_else(|| panic!("PickHead cannot reach head = {head}"));
        for l in 2..=head {
            let Some(trailed) = pick(&headed, "PickTrail", "l", l) else {
                assert!(head - l >= viewport, "only an off-screen tail is refused");
                continue;
            };
            for (mt, vb) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let ticked = model
                    .successors("PickTick", &trailed)
                    .into_iter()
                    .find(|s| s.get("mt") == Some(&mt) && s.get("vb") == Some(&vb))
                    .expect("every tick");
                let (was_cells, now_cells) = (
                    column(head - l..=head - 1, 0),
                    column(head - l + 1..=head, mt),
                );
                let mut prev = term.cell_frame(rows, COLS);
                rain.publish(&was_cells, &tiles, &mut prev.fx_layers);
                let mut cur = term.cell_frame(rows, COLS);
                let now_tiles = if vb == 1 { &rebaked } else { &tiles };
                rain.publish(&now_cells, now_tiles, &mut cur.fx_layers);
                let mut dirty = Vec::new();
                let DirtyDecision::Rows(_) = compute_dirty_rows(
                    &prev,
                    &cur,
                    false,
                    None,
                    false,
                    None,
                    aterm_render::RowBands::new(0, 16),
                    &mut dirty,
                ) else {
                    panic!("identical-geometry frames must take the row-damage path");
                };
                let (was, now) = (rows_of(&was_cells), rows_of(&now_cells));
                for r in head - l..=head.min(viewport - 1) {
                    let probed = pick(&ticked, "PickRow", "r", r).expect("a visible band row");
                    let row = r as u16;
                    let mut after = probed.clone();
                    after.insert("changed", i64::from(was.get(&row) != now.get(&row)));
                    after.insert("marked", i64::from(dirty[r as usize]));
                    after.insert("phase", 5);
                    assert_eq!(
                        model.successors("Mark", &probed),
                        vec![after.clone()],
                        "row {r} of head={head} l={l} mt={mt} vb={vb}: the real verdict is \
                         not the model's Mark (dirty={dirty:?})"
                    );
                    rebuilt += usize::from(after["marked"] == 1 && after["changed"] == 0);
                    if buggy.successors("Mark", &probed) != vec![after] {
                        separating += 1;
                    }
                    marked_rows += 1;
                }
            }
        }
    }
    assert!(
        marked_rows > 100,
        "the lattice must be walked ({marked_rows} rows)"
    );
    assert!(
        separating > 0,
        "the lattice must reach rows the Buggy = 1 marker decides differently"
    );
    assert!(
        rebuilt > 0,
        "the lattice must reach rows a re-bake rebuilds with the same cells ({rebuilt})"
    );
}

#[test]
fn real_rain_emission_leaves_no_changed_row_unmarked() {
    use aterm_effects::matrix_rain::{EffectGeom, MatrixRain, RainConfig, RainTickInput};
    const ROWS: usize = 12;
    const COLS: usize = 16;
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"\x1b[?25l");
    let base = term.cell_frame(ROWS, COLS);
    let geom = EffectGeom {
        cell_w: 8,
        cell_h: 16,
        rows: ROWS as u16,
        cols: COLS as u16,
    };
    let mut engine = MatrixRain::new(RainConfig {
        enabled: true,
        density: 12,
        output_material: false,
        seed: 7,
        ..RainConfig::default()
    });
    engine.rescan_from_cells(
        &base.cells,
        &base.line_sizes,
        &base.images,
        ROWS,
        COLS,
        base.default_bg,
        1,
    );
    let mut rain = RainLayers::default();
    let mut cells = Vec::new();
    // The cells that draw: a cell whose glyph is not baked yet draws nothing.
    let drawn = |cells: &[RainCell], tiles: &RainBaker| -> Vec<RainCell> {
        cells
            .iter()
            .filter(|c| c.alpha > 0 && tiles.tile(c.glyph).is_some())
            .copied()
            .collect()
    };
    let (mut prev, mut prev_cells, mut prev_version) = (base.clone(), Vec::new(), 0u64);
    let (mut changed_total, mut row_frames) = (0usize, 0usize);
    for tick in 0..240u64 {
        engine.note_activity(tick + 1);
        engine.advance_ms(33);
        let mut cur = base.clone();
        engine.emit(
            geom,
            &RainTickInput::default(),
            &mut cells,
            &mut cur.rain_add,
        );
        rain.publish(&cells, engine.baker(), &mut cur.fx_layers);
        let mut dirty = Vec::new();
        if let DirtyDecision::Rows(_) = compute_dirty_rows(
            &prev,
            &cur,
            false,
            None,
            false,
            None,
            aterm_render::RowBands::new(0, 16),
            &mut dirty,
        ) {
            row_frames += 1;
            let rebaked = prev_version != engine.atlas_version();
            let now_cells = drawn(&cells, engine.baker());
            let (was, now) = (rows_of(&prev_cells), rows_of(&now_cells));
            for row in was.keys().chain(now.keys()) {
                if rebaked || was.get(row) != now.get(row) {
                    changed_total += 1;
                    assert!(
                        dirty[usize::from(*row)],
                        "tick {tick}: rain row {row} changed but was not marked"
                    );
                }
            }
        }
        prev = cur;
        prev_cells = drawn(&cells, engine.baker());
        prev_version = engine.atlas_version();
    }
    assert!(row_frames > 0, "the downpour must take the row-damage path");
    assert!(changed_total > 0, "the downpour must actually change rows");
}
