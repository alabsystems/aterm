// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Digital rain tests for the CPU renderer.

use std::sync::Arc;

use aterm_core::render::{RainHalo, SceneAtlas, SpriteQuad};
use aterm_core::terminal::Terminal;
use aterm_render::{
    DirtyDecision, Frame, Renderer, Theme, WindowCpu, compute_dirty_rows, rgb_to_u32,
};

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

fn patterned_atlas(w: u32, h: u32, version: u64) -> SceneAtlas {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            rgba.extend_from_slice(&[
                (x * 37 + y * 11) as u8,
                (x * 5 + y * 53) as u8,
                (x * 29 + y * 3) as u8,
                255,
            ]);
        }
    }
    SceneAtlas {
        width: w,
        height: h,
        rgba,
        version,
    }
}

fn solid_atlas(w: u32, h: u32, rgb: [u8; 3], alpha: u8, version: u64) -> SceneAtlas {
    let rgba = (0..w * h)
        .flat_map(|_| [rgb[0], rgb[1], rgb[2], alpha])
        .collect();
    SceneAtlas {
        width: w,
        height: h,
        rgba,
        version,
    }
}

fn quad(row: u16, dest: [u16; 4], src: [u16; 4]) -> SpriteQuad {
    SpriteQuad {
        row,
        x: dest[0],
        y: dest[1],
        w: dest[2],
        h: dest[3],
        ax: src[0],
        ay: src[1],
        aw: src[2],
        ah: src[3],
        tint: 0x00FF_FFFF,
        alpha: 255,
        flip_x: false,
    }
}

fn band_quad(row: u16, x: u16, cw: u16, ch: u16, ax: u16, ay: u16) -> SpriteQuad {
    quad(row, [x, row * ch, cw, ch], [ax, ay, cw, ch])
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
    input.rain_atlas = Some(Arc::new(solid_atlas(64, 64, [200, 200, 200], 255, 1)));
    let mut q = band_quad(1, 0, cw as u16, ch.min(64) as u16, 0, 0);
    q.tint = 0x0080_40C0;
    input.rain_quads = vec![q];
    let f = rend.render_input(&input);
    let want = (mul8(200, 0x80) << 16) | (mul8(200, 0x40) << 8) | mul8(200, 0xC0);
    let band_h = ch.min(64);
    for dy in 0..band_h {
        for dx in 0..cw {
            let got = f.pixels[(ch + dy) * f.width + dx];
            assert_eq!(
                got, want,
                "tinted opaque stamp must be the exact mul8 product at ({dx},{dy})"
            );
        }
    }
    for (i, (&b, &p)) in base.iter().zip(f.pixels.iter()).enumerate() {
        if b != p {
            let y = i / f.width;
            assert!(
                (ch..ch + band_h).contains(&y),
                "rain stamp must stay inside its cell band: pixel row {y} changed"
            );
        }
    }

    let mut input = term.cell_frame(4, 12);
    input.rain_atlas = Some(Arc::new(solid_atlas(64, 64, [255, 255, 255], 127, 2)));
    let mut q = band_quad(1, 0, cw as u16, ch.min(64) as u16, 0, 0);
    q.alpha = 1;
    input.rain_quads = vec![q];
    let f = rend.render_input(&input);
    assert_eq!(
        base, f.pixels,
        "mul8(127, 1) == 0: a fully-rounded-away alpha must paint nothing"
    );
    let mut input = term.cell_frame(4, 12);
    input.rain_atlas = Some(Arc::new(solid_atlas(64, 64, [255, 255, 255], 128, 3)));
    input.rain_quads = vec![q];
    let f = rend.render_input(&input);
    assert_ne!(
        base, f.pixels,
        "mul8(128, 1) == 1: one unit of coverage must survive the rounding"
    );

    let atlas = patterned_atlas(64, 64, 4);
    let (aw, ah) = (cw.min(64) as u16, ch.min(64) as u16);
    let mut input = term.cell_frame(4, 12);
    input.rain_atlas = Some(Arc::new(patterned_atlas(64, 64, 4)));
    let mut q = quad(2, [3, 2 * ch as u16, aw, ah], [5, 7, aw, ah]);
    q.flip_x = true;
    input.rain_quads = vec![q];
    let f = rend.render_input(&input);
    for dy in 0..ah as usize {
        for dx in 0..aw as usize {
            let sx = 5 + (aw as usize - 1 - dx);
            let i = ((7 + dy) * 64 + sx) * 4;
            let want = ((atlas.rgba[i] as u32) << 16)
                | ((atlas.rgba[i + 1] as u32) << 8)
                | atlas.rgba[i + 2] as u32;
            let got = f.pixels[(2 * ch + dy) * f.width + 3 + dx];
            assert_eq!(
                got, want,
                "flip_x must mirror the source column at ({dx},{dy})"
            );
        }
    }
}

#[test]
fn rain_under_text_and_under_cats() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let mut term = Terminal::new(2, 8);
    term.process("\x1b[?25l█".as_bytes());

    let rain_green = 0x0020_C840;
    let cat_red = 0x00C8_2020;
    let mut input = term.cell_frame(2, 8);
    input.rain_atlas = Some(Arc::new(solid_atlas(8, 8, [0x20, 0xC8, 0x40], 255, 1)));
    input.rain_quads = vec![quad(0, [0, 0, (3 * cw) as u16, ch as u16], [0, 0, 8, 8])];
    input.cat_atlas = Some(Arc::new(solid_atlas(8, 8, [0xC8, 0x20, 0x20], 255, 1)));
    input.cat_quads = vec![quad(
        0,
        [(2 * cw) as u16, 0, cw as u16, ch as u16],
        [0, 0, 8, 8],
    )];

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
        "the cat must draw OVER the rain (cell 2 pure red — cats walk on rain)"
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
    let (cw16, ch16) = (cw as u16, ch.min(48) as u16);

    let mut make = |quads: Vec<SpriteQuad>, add: Vec<RainHalo>, atlas: &Arc<SceneAtlas>| {
        let mut input = term.cell_frame(rows, 12);
        input.rain_atlas = Some(atlas.clone());
        input.rain_quads = quads;
        input.rain_add = add;
        input
    };
    let atlas_v1 = Arc::new(patterned_atlas(64, 64, 1));

    let in_a = make(
        vec![
            band_quad(1, 0, cw16, ch16, 0, 0),
            band_quad(1, 3 * cw16, cw16, ch16, 8, 0),
            band_quad(2, 3 * cw16, cw16, ch16, 8, 16),
        ],
        vec![halo(2, 3 * cw16, 2 * ch16, cw16, ch16, 0x0030_6030)],
        &atlas_v1,
    );
    let in_b = make(
        vec![
            band_quad(2, 0, cw16, ch16, 0, 0),
            band_quad(2, 3 * cw16, cw16, ch16, 8, 16),
            band_quad(3, 3 * cw16, cw16, ch16, 8, 32),
        ],
        vec![halo(3, 3 * cw16, 3 * ch16, cw16, ch16, 0x0030_6030)],
        &atlas_v1,
    );
    let in_c = make(
        (0..rows as u16)
            .map(|r| band_quad(r, cw16, cw16, ch16, 16 + r, 8))
            .collect(),
        vec![halo(5, cw16, 5 * ch16, cw16, ch16, 0x0018_4018)],
        &atlas_v1,
    );
    let atlas_v2 = {
        let mut a = patterned_atlas(64, 64, 2);
        for px in a.rgba.as_chunks_mut::<4>().0 {
            px[0] = px[0].wrapping_add(64);
        }
        Arc::new(a)
    };
    let in_d = make(in_c.rain_quads.clone(), in_c.rain_add.clone(), &atlas_v2);

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

#[test]
fn rain_dirty_gate_and_merge_diff_row_marking() {
    let mut term = Terminal::new(6, 8);
    term.process(b"\x1b[?25l"); // hidden cursor: no cursor rows in the dirty set
    let atlas_v1 = Arc::new(patterned_atlas(16, 16, 1));
    let settled = vec![
        quad(1, [0, 16, 8, 16], [0, 0, 8, 16]),
        quad(2, [0, 32, 8, 16], [0, 0, 8, 16]),
        quad(2, [24, 32, 8, 16], [8, 0, 8, 16]),
        quad(3, [8, 48, 8, 16], [0, 0, 8, 16]),
    ];
    let halos = vec![halo(4, 0, 64, 8, 16, 0x0010_2010)];

    let mut frame = |quads: &[SpriteQuad], add: &[RainHalo], atlas: &Arc<SceneAtlas>| {
        let mut input = term.cell_frame(6, 8);
        input.rain_atlas = Some(atlas.clone());
        input.rain_quads = quads.to_vec();
        input.rain_add = add.to_vec();
        input
    };
    let marked = |dirty: &[bool]| -> Vec<usize> {
        dirty
            .iter()
            .enumerate()
            .filter_map(|(r, &b)| b.then_some(r))
            .collect()
    };

    let prev = frame(&settled, &halos, &atlas_v1);
    let cur = frame(&settled, &halos, &atlas_v1);
    let mut dirty = Vec::new();
    let DirtyDecision::Rows(d) = compute_dirty_rows(
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
    assert!(!d.rain_changed, "settled rain must not set rain_changed");
    assert!(
        !d.rain_add_changed,
        "settled halos must not set rain_add_changed"
    );
    assert!(
        d.is_gate_hit(),
        "settled rain must gate-hit: steady state is free"
    );
    assert!(dirty.iter().all(|&b| !b), "settled rain must mark no rows");

    let moved = vec![
        settled[0],
        settled[1],
        settled[3],
        quad(4, [8, 64, 8, 16], [8, 0, 8, 16]),
    ];
    let cur = frame(&moved, &halos, &atlas_v1);
    let DirtyDecision::Rows(d) = compute_dirty_rows(
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
    assert!(d.rain_changed, "moved quads must set rain_changed");
    assert!(!d.is_gate_hit(), "changed rain must NOT gate-hit");
    assert_eq!(
        marked(&dirty),
        vec![2, 3, 4],
        "merge-diff marks the differing rows; the band fill adds only the \
         settled in-band row — the out-of-band settled row 1 stays clean"
    );

    let mut mutated = settled.clone();
    mutated[3].ax = 8;
    let cur = frame(&mutated, &halos, &atlas_v1);
    let DirtyDecision::Rows(d) = compute_dirty_rows(
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
    assert!(d.rain_changed);
    assert_eq!(
        marked(&dirty),
        vec![3],
        "a within-row slice change must mark exactly that row"
    );

    let cur = frame(&settled, &halos, &Arc::new(patterned_atlas(16, 16, 2)));
    let DirtyDecision::Rows(d) = compute_dirty_rows(
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
    assert!(
        d.rain_changed,
        "an atlas-version bump alone must set rain_changed (a rebake repaints)"
    );
    assert!(!d.is_gate_hit(), "a rebaked atlas must NOT gate-hit");
    assert_eq!(
        marked(&dirty),
        vec![1, 2, 3],
        "the rebake must mark every (unmoved) quad row"
    );

    let cur = frame(&settled, &[halo(5, 0, 80, 8, 16, 0x0010_2010)], &atlas_v1);
    let DirtyDecision::Rows(d) = compute_dirty_rows(
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
    assert!(!d.rain_changed, "settled quads must not set rain_changed");
    assert!(d.rain_add_changed, "a moved halo must set rain_add_changed");
    assert!(!d.is_gate_hit());
    assert_eq!(
        marked(&dirty),
        vec![4, 5],
        "a moved halo must mark exactly its prev∪cur rows"
    );
}

fn rows_of(quads: &[SpriteQuad]) -> std::collections::BTreeMap<u16, Vec<SpriteQuad>> {
    let mut rows = std::collections::BTreeMap::<u16, Vec<SpriteQuad>>::new();
    for q in quads {
        rows.entry(q.row).or_default().push(*q);
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
    let glyph = |row: i64, window: i64| ((row * 3 + window * 5) % 8) as u16;
    let mut column = |lit: std::ops::RangeInclusive<i64>, window: i64| {
        let mut input = term.cell_frame(rows, COLS);
        input.rain_quads = lit
            .filter(|&r| r < viewport)
            .map(|r| band_quad(r as u16, 8, 8, 16, glyph(r, window) * 8, 0))
            .collect();
        input
    };
    let (mut marked_rows, mut separating) = (0usize, 0usize);
    for head in 2..=viewport + 2 {
        let headed = pick(&model.init_state(), "PickHead", "head", head)
            .unwrap_or_else(|| panic!("PickHead cannot reach head = {head}"));
        for l in 2..=head {
            let Some(trailed) = pick(&headed, "PickTrail", "l", l) else {
                assert!(head - l >= viewport, "only an off-screen tail is refused");
                continue;
            };
            for mt in 0..=1i64 {
                let ticked = pick(&trailed, "PickTick", "mt", mt).expect("both ticks");
                let prev = column(head - l..=head - 1, 0);
                let cur = column(head - l + 1..=head, mt);
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
                let (was, now) = (rows_of(&prev.rain_quads), rows_of(&cur.rain_quads));
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
                        "row {r} of head={head} l={l} mt={mt}: the real verdict is not the \
                         model's Mark (dirty={dirty:?})"
                    );
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
    let (mut prev, mut changed_total, mut row_frames) = (base.clone(), 0usize, 0usize);
    for tick in 0..240u64 {
        engine.note_activity(tick + 1);
        engine.advance_ms(33);
        let mut cur = base.clone();
        engine.emit(
            geom,
            &RainTickInput::default(),
            &mut cur.rain_quads,
            &mut cur.rain_add,
        );
        cur.rain_atlas = engine.rain_atlas();
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
            let rebaked = prev.rain_atlas.as_ref().map(Arc::as_ptr)
                != cur.rain_atlas.as_ref().map(Arc::as_ptr);
            let (was, now) = (rows_of(&prev.rain_quads), rows_of(&cur.rain_quads));
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
    }
    assert!(row_frames > 0, "the downpour must take the row-damage path");
    assert!(changed_total > 0, "the downpour must actually change rows");
}
