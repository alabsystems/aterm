// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_core::terminal::{CursorStyle, Terminal};
use aterm_render::{
    ColumnGlyph, Frame, LigatureMode, Renderer, Scale, TextShapingConfig, Theme, clip_span,
    cursor_cutout_cols, for_each_cursor_rect, glyph_quad,
};

const CURSOR: u32 = 0x0050_FA7B; // Theme::default().cursor

fn ligature_test_font() -> Vec<u8> {
    if let Ok(path) = std::env::var("ATERM_LIGATURE_TEST_FONT")
        && let Ok(bytes) = std::fs::read(&path)
    {
        return bytes;
    }
    include_bytes!("fixtures/jetbrains-mono.ttf").to_vec()
}

fn renderer() -> Renderer {
    let mut r = Renderer::from_bytes(&ligature_test_font(), 18.0, Theme::default())
        .expect("the ligature test font parses");
    r.set_text_shaping(TextShapingConfig {
        ligature_mode: LigatureMode::Enabled,
        ..Default::default()
    });
    r
}

const ROWS: usize = 1;
const COLS: usize = 12;
const TEXT: &[u8] = "a=>b \u{65E5}x".as_bytes(); // 日 = U+65E5 (wide)

fn frame_with_cursor(r: &mut Renderer, col: Option<usize>) -> Frame {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"\x1b[2 q"); // steady block (override below can reshape)
    term.process(TEXT);
    match col {
        Some(c) => {
            term.process(format!("\x1b[1;{}H", c + 1).as_bytes());
        }
        None => {
            term.process(b"\x1b[?25l"); // DECTCEM hide: the no-cursor frame
        }
    }
    r.render_input(&term.cell_frame(ROWS, COLS))
}

fn cursor_rect_union(
    style: CursorStyle,
    col: usize,
    cw: usize,
    ch: usize,
    wide_lead: bool,
) -> Vec<[usize; 4]> {
    let is_block = matches!(style, CursorStyle::BlinkingBlock | CursorStyle::SteadyBlock);
    let cur_w = if is_block && wide_lead { 2 * cw } else { cw };
    let mut rects = Vec::new();
    for_each_cursor_rect(style, col * cw, 0, cur_w, ch, |rect| rects.push(rect));
    rects
}

fn in_rects(rects: &[[usize; 4]], x: usize, y: usize) -> bool {
    rects
        .iter()
        .any(|&[rx, ry, rw, rh]| x >= rx && x < rx + rw && y >= ry && y < ry + rh)
}

#[test]
fn cursor_touches_only_its_rect_all_columns_and_styles() {
    let mut r = renderer();
    let (cw, ch) = r.cell_size();
    let hidden = frame_with_cursor(&mut r, None);
    let styles = [
        CursorStyle::SteadyBlock,
        CursorStyle::SteadyUnderline,
        CursorStyle::SteadyBar,
        CursorStyle::HollowBlock,
        CursorStyle::Bolt,
    ];
    let mut any_inside_changed = false;
    for style in styles {
        r.set_cursor_style_override(Some(style));
        for col in 0..COLS {
            let f = frame_with_cursor(&mut r, Some(col));
            assert_eq!(
                (f.width, f.height),
                (hidden.width, hidden.height),
                "cursor must not change frame dimensions"
            );
            let mut term = Terminal::new(ROWS as u16, COLS as u16);
            term.process(TEXT);
            let input = term.cell_frame(ROWS, COLS);
            let wide_lead = input.cells[0].get(col + 1).is_some_and(|n| n.wide);
            let rects = cursor_rect_union(style, col, cw, ch, wide_lead);
            for y in 0..f.height {
                for x in 0..f.width {
                    let idx = y * f.width + x;
                    if in_rects(&rects, x, y) {
                        if f.pixels[idx] != hidden.pixels[idx] {
                            any_inside_changed = true;
                        }
                    } else {
                        assert_eq!(
                            f.pixels[idx], hidden.pixels[idx],
                            "cursor ink BLED outside its rect: style {style:?} col {col} \
                             pixel ({x},{y}) changed vs the no-cursor frame"
                        );
                    }
                }
            }
        }
    }
    r.set_cursor_style_override(None);
    assert!(
        any_inside_changed,
        "NON-VACUITY: no cursor changed any pixel — the sweep never drew a cursor"
    );
}

#[test]
fn block_on_ligature_lead_recolors_covering_slice() {
    let mut r = renderer();
    let (cw, ch) = r.cell_size();
    let hidden = frame_with_cursor(&mut r, None);
    let f = frame_with_cursor(&mut r, Some(1)); // '=' cell of '=>'
    let band = |fr: &Frame, c: usize| -> Vec<u32> {
        let mut out = Vec::with_capacity(cw * ch);
        for y in 0..fr.height {
            out.extend_from_slice(&fr.pixels[y * fr.width + c * cw..y * fr.width + (c + 1) * cw]);
        }
        out
    };
    let cell = band(&f, 1);
    assert!(
        cell.contains(&CURSOR),
        "the '=' cell must show the block cursor fill"
    );
    assert!(
        cell.iter().any(|&p| p != CURSOR),
        "the covering arrow glyph's slice over the cursor cell must be recoloured \
         (cut out) — pre-W4 the empty placeholder produced no cut-out at all"
    );
    assert_eq!(
        band(&f, 2),
        band(&hidden, 2),
        "the arrow ink outside the cursor rect must survive the lead-cell cursor"
    );
}

#[test]
fn block_on_wide_lead_inverts_both_cells() {
    let mut r = renderer();
    let (cw, ch) = r.cell_size();
    let hidden = frame_with_cursor(&mut r, None);
    let f = frame_with_cursor(&mut r, Some(5)); // 日 lead cell
    let band = |fr: &Frame, c: usize| -> Vec<u32> {
        let mut out = Vec::with_capacity(cw * ch);
        for y in 0..fr.height {
            out.extend_from_slice(&fr.pixels[y * fr.width + c * cw..y * fr.width + (c + 1) * cw]);
        }
        out
    };
    assert!(
        band(&f, 5).contains(&CURSOR),
        "the wide LEAD cell must show the block cursor fill"
    );
    assert!(
        band(&f, 6).contains(&CURSOR),
        "the wide CONTINUATION cell must show the block cursor fill — the \
         single-cell fill left the right half of the ideograph erased pre-W4"
    );
    assert_eq!(band(&f, 4), band(&hidden, 4), "left neighbour untouched");
    assert_eq!(band(&f, 7), band(&hidden, 7), "right neighbour untouched");
}

#[test]
fn clip_span_slices_tile_exactly() {
    const LO: i64 = -1_000; // widened sentinels (the blit callers use i32::MIN/MAX)
    const HI: i64 = 1_000;
    let mut three_way = 0usize;
    let mut bug_caught = 0usize;
    for gx0 in -6i64..=10 {
        for len in 0i64..=8 {
            for c0 in -6i64..=12 {
                for c1 in -6i64..=12 {
                    let left = clip_span(gx0, len, LO, c0.min(c1));
                    let mid = clip_span(gx0, len, c0.min(c1), c1.max(c0));
                    let right = clip_span(gx0, len, c1.max(c0), HI);
                    let inside =
                        |s: Option<(i64, i64)>, x: i64| s.is_some_and(|(lo, hi)| x >= lo && x < hi);
                    for x in gx0..gx0 + len {
                        let n = usize::from(inside(left, x))
                            + usize::from(inside(mid, x))
                            + usize::from(inside(right, x));
                        assert_eq!(
                            n, 1,
                            "x={x} covered by {n} slices (gx0={gx0} len={len} \
                             window=[{c0},{c1})) — slices must tile the extent exactly"
                        );
                    }
                    for s in [left, mid, right].into_iter().flatten() {
                        assert!(s.0 >= gx0 && s.1 <= gx0 + len, "span exits the glyph");
                    }
                    if let Some((mlo, mhi)) = mid {
                        assert!(
                            mlo >= c0.min(c1) && mhi <= c1.max(c0),
                            "cut-out slice bleeds outside the cursor window"
                        );
                    }
                    if left.is_some() && mid.is_some() && right.is_some() {
                        three_way += 1;
                    }
                    let unclipped = (len > 0).then_some((gx0, gx0 + len));
                    if let Some((ulo, uhi)) = unclipped
                        && mid != unclipped
                        && !(ulo >= c0.min(c1) && uhi <= c1.max(c0))
                    {
                        bug_caught += 1;
                    }
                }
            }
        }
    }
    assert!(
        three_way > 0,
        "NON-VACUITY: no case produced all three slices"
    );
    assert!(
        bug_caught > 0,
        "NEGATIVE CONTROL: the unclipped pre-fix cut-out never violated no-bleed \
         on this lattice — the lattice is too weak to catch the original bug"
    );
}

#[test]
fn glyph_quad_x_slices_tile_and_preserve_texels() {
    let (aw, ah) = (64.0f32, 64.0f32);
    let (ax, ay, gh, ymin, baseline) = (7u32, 3u32, 4u32, 0i32, 12i32);
    let texel_at = |rect: [f32; 4], uv: [f32; 4], x: i32| -> i32 {
        let fx = (x as f32 + 0.5 - rect[0]) / rect[2];
        ((uv[0] + fx * uv[2]) * aw).floor() as i32
    };
    let covered =
        |rect: [f32; 4]| -> std::ops::Range<i32> { (rect[0] as i32)..((rect[0] + rect[2]) as i32) };
    let mut sliced_cases = 0usize;
    for xs in [1usize, 2] {
        let scale = Scale {
            xs,
            ..Scale::NORMAL
        };
        for gw in 1u32..=6 {
            for xmin in -3i32..=3 {
                for cell_left in [0.0f32, 5.0] {
                    let Some((rect0, uv0)) = glyph_quad(
                        cell_left, 0, baseline, scale, ax, ay, gw, gh, xmin, ymin, aw, ah,
                    ) else {
                        continue;
                    };
                    let full = covered(rect0);
                    for c0 in -4i32..=16 {
                        for c1 in c0..=16 {
                            let windows = [(i32::MIN, c0), (c0, c1), (c1, i32::MAX)];
                            let mut seen = std::collections::HashMap::new();
                            for (lo, hi) in windows {
                                let clipped = Scale {
                                    clip_x0: lo,
                                    clip_x1: hi,
                                    ..scale
                                };
                                let Some((rect, uv)) = glyph_quad(
                                    cell_left, 0, baseline, clipped, ax, ay, gw, gh, xmin, ymin,
                                    aw, ah,
                                ) else {
                                    continue;
                                };
                                for x in covered(rect) {
                                    let t = texel_at(rect, uv, x);
                                    assert!(
                                        seen.insert(x, t).is_none(),
                                        "pixel x={x} covered by two slices (overlap)"
                                    );
                                }
                            }
                            for x in full.clone() {
                                let want = texel_at(rect0, uv0, x);
                                match seen.remove(&x) {
                                    Some(got) => assert_eq!(
                                        got, want,
                                        "slice samples a DIFFERENT texel at x={x} \
                                         (xs={xs} gw={gw} xmin={xmin} window=[{c0},{c1}))"
                                    ),
                                    None => panic!(
                                        "pixel x={x} lost by slicing (xs={xs} gw={gw} \
                                         xmin={xmin} window=[{c0},{c1}))"
                                    ),
                                }
                            }
                            assert!(
                                seen.is_empty(),
                                "slices cover pixels the whole quad does not: {seen:?}"
                            );
                            if c0 > full.start && c1 < full.end && c0 < c1 {
                                sliced_cases += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(
        sliced_cases > 0,
        "NON-VACUITY: no lattice case actually produced a 3-way slice"
    );
}

#[test]
fn cursor_cutout_cols_exhaustive() {
    const N: usize = 9;
    for mask in 0u32..(1 << N) {
        let plan: Vec<ColumnGlyph> = (0..N)
            .map(|c| {
                if mask & (1 << c) != 0 {
                    ColumnGlyph::Ligated(c as u16) // gid value is irrelevant
                } else {
                    ColumnGlyph::PerCell
                }
            })
            .collect();
        let lig = |c: usize| matches!(plan.get(c), Some(ColumnGlyph::Ligated(_)));
        for cc in 0..N {
            let (lo, hi) = cursor_cutout_cols(&plan, cc);
            assert!(lo <= cc && cc <= hi, "cc must lie inside the range");
            if lig(cc) {
                assert!((lo..=hi).all(lig), "range must be all-Ligated");
                assert!(lo == 0 || !lig(lo - 1), "range must be left-maximal");
                assert!(!lig(hi + 1), "range must be right-maximal");
            } else {
                assert_eq!((lo, hi), (cc, cc), "non-ligated cursor cuts only itself");
            }
        }
        assert_eq!(cursor_cutout_cols(&plan, N + 3), (N + 3, N + 3));
    }
}
