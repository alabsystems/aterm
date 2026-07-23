// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// W4 — CURSOR INK INTEGRITY (partition / no-bleed). Two-tier proof:
//
//  * Tier-1 (this file, plain cargo): the PIXEL-LEVEL invariant on the real
//    renderer — "cursor rendering writes ONLY within the cursor rect; the
//    complement of the rect is byte-identical to the no-cursor frame" — swept
//    over every cursor column × style on a row mixing a ligature ('=>'), a
//    wide (CJK) lead + continuation and plain cells; plus the two audit pins
//    (cursor on the '>' of '=>' leaves the '=' cell untouched; cursor on a
//    CJK lead inverts BOTH cells). The pure clip/slicing seams (`clip_span`,
//    `cursor_cutout_cols`, `glyph_quad`'s x-clip) are proven by exhaustive
//    lattice enumeration with non-vacuity + pre-fix negative controls.
//  * Tier-0: the derived ty model `cursor_cutout_clip_model`
//    (crates/aterm-spec/src/derive.rs) carries the SAME tiling + no-bleed
//    invariant abstractly; `derived_ring_ty.rs` has the real `ty` prove it at
//    Buggy=0 and REQUIRES a counterexample at Buggy=1 (the pre-W4 unclipped
//    cut-out that repainted a ligature's lead cells in bg).
//
// GPU: the identical geometry drives the quad slicing (`glyph_quad` with
// `Scale::clip_x0/clip_x1`); crates/aterm-gpu/tests/ligature_parity.rs binds
// the GPU frames to these CPU frames pixel-for-pixel (<=8 LSB blend tolerance).

use aterm_core::terminal::{CursorStyle, Terminal};
use aterm_render::{
    ColumnGlyph, Frame, LigatureMode, Renderer, Scale, TextShapingConfig, Theme, clip_span,
    cursor_cutout_cols, cursor_rects, glyph_quad,
};

const CURSOR: u32 = 0x0050_FA7B; // Theme::default().cursor
const BG: u32 = 0x0011_1318; // Theme::default().bg

// Layout-independent ligature font discovery (same as tests/ligatures.rs):
// $ATERM_FONT if set, else the committed JetBrains Mono fixture. None -> SKIP.
fn ligature_test_font() -> Option<Vec<u8>> {
    if let Ok(path) = std::env::var("ATERM_FONT")
        && let Ok(bytes) = std::fs::read(&path)
    {
        return Some(bytes);
    }
    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/jetbrains-mono.ttf"
    );
    std::fs::read(FIXTURE).ok()
}

fn renderer() -> Option<Renderer> {
    let bytes = ligature_test_font()?;
    let mut r = Renderer::from_bytes(&bytes, 18.0, Theme::default()).ok()?;
    r.set_text_shaping(TextShapingConfig {
        ligature_mode: LigatureMode::Enabled,
        ..Default::default()
    });
    Some(r)
}

const ROWS: usize = 1;
const COLS: usize = 12;
// One row exercising every cut-out shape: a ligature run ('=>' at cols 1-2), a
// plain cell, a wide CJK lead + continuation (cols 5-6) and trailing blanks.
const TEXT: &[u8] = "a=>b \u{65E5}x".as_bytes(); // 日 = U+65E5 (wide)

/// Render TEXT with the cursor placed at `col` under DECSCUSR `style_code`
/// (2=block, 4=underline, 6=bar), or hidden entirely when `col` is None.
fn frame_with_cursor(r: &mut Renderer, col: Option<usize>) -> Frame {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"\x1b[2 q"); // steady block (override below can reshape)
    term.process(TEXT);
    match col {
        Some(c) => {
            // CUP is 1-based; land the cursor on viewport col `c` of row 0.
            term.process(format!("\x1b[1;{}H", c + 1).as_bytes());
        }
        None => {
            term.process(b"\x1b[?25l"); // DECTCEM hide: the no-cursor frame
        }
    }
    r.render_input(&term.cell_frame(ROWS, COLS))
}

/// The exact pixel rects the cursor paints for `style` at `col` — the SAME
/// geometry the renderer uses: `cursor_rects` over the (block-widened on a
/// wide lead) cursor width. `wide_lead` mirrors `draw_cursor`'s probe.
fn cursor_rect_union(
    style: CursorStyle,
    col: usize,
    cw: usize,
    ch: usize,
    wide_lead: bool,
) -> Vec<[usize; 4]> {
    let is_block = matches!(style, CursorStyle::BlinkingBlock | CursorStyle::SteadyBlock);
    let cur_w = if is_block && wide_lead { 2 * cw } else { cw };
    cursor_rects(style, col * cw, 0, cur_w, ch)
}

fn in_rects(rects: &[[usize; 4]], x: usize, y: usize) -> bool {
    rects
        .iter()
        .any(|&[rx, ry, rw, rh]| x >= rx && x < rx + rw && y >= ry && y < ry + rh)
}

/// THE PARTITION / NO-BLEED INVARIANT, swept: for EVERY cursor column and every
/// steady cursor style, each pixel OUTSIDE the cursor rect is byte-identical to
/// the cursor-hidden frame. Pre-W4 this failed on three counts: a block cursor
/// on the '>' of '=>' repainted the arrow's lead-cell ink in bg; a block cursor
/// on a CJK lead erased the ideograph's right half; and the fill/cut-out pair
/// mismatched on wide leads. Non-vacuity: at least one column must CHANGE
/// pixels inside its rect (the cursor is really drawn).
#[test]
fn cursor_touches_only_its_rect_all_columns_and_styles() {
    let Some(mut r) = renderer() else {
        eprintln!("SKIP: no ligature test font (set ATERM_FONT or add the repo fixture)");
        return;
    };
    let (cw, ch) = r.cell_size();
    let hidden = frame_with_cursor(&mut r, None);
    let styles = [
        CursorStyle::SteadyBlock,
        CursorStyle::SteadyUnderline,
        CursorStyle::SteadyBar,
        CursorStyle::HollowBlock,
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
            // The wide-lead probe mirrors draw_cursor: the NEXT cell is wide.
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

/// AUDIT PIN: a block cursor on the '>' of '=>' leaves the '=' cell
/// byte-identical to the no-cursor frame — AND that cell genuinely carries the
/// ligature's lead ink (non-bg pixels), so the assert is non-vacuous. Pre-W4
/// the unclipped cut-out repainted the arrow's lead-cell ink in bg (invisible
/// ink), which this pin catches.
#[test]
fn block_on_ligature_tail_leaves_lead_cell_identical() {
    let Some(mut r) = renderer() else {
        eprintln!("SKIP: no ligature test font (set ATERM_FONT or add the repo fixture)");
        return;
    };
    let (cw, ch) = r.cell_size();
    let hidden = frame_with_cursor(&mut r, None);
    let cursor_on_gt = frame_with_cursor(&mut r, Some(2)); // '>' cell of '=>'
    let band = |f: &Frame, c: usize| -> Vec<u32> {
        let mut out = Vec::with_capacity(cw * ch);
        for y in 0..f.height {
            out.extend_from_slice(&f.pixels[y * f.width + c * cw..y * f.width + (c + 1) * cw]);
        }
        out
    };
    // Non-vacuity: the '=' cell shows ligature ink in the no-cursor frame.
    assert!(
        band(&hidden, 1).iter().any(|&p| p != BG),
        "the '=' cell of '=>' must carry ligature ink — is the fixture ligating?"
    );
    assert_eq!(
        band(&cursor_on_gt, 1),
        band(&hidden, 1),
        "block cursor on '>' must leave the '=' cell byte-identical to the \
         no-cursor frame — the cut-out repainted the ligature's lead ink"
    );
    // And the cursor cell itself really shows the cursor.
    assert!(
        band(&cursor_on_gt, 2).contains(&CURSOR),
        "the '>' cell must show the block cursor fill"
    );
}

/// Cursor on a ligature LEAD cell: the covering glyph's slice over the cursor
/// cell is recoloured (cut out) while the rest of the arrow survives. Pre-W4
/// the lead's empty placeholder produced NO cut-out, hiding the arrow's lead
/// ink entirely under the fill. Assert the cursor cell shows BOTH the fill and
/// recoloured ink, and (from the sweep invariant) the '>' cell is untouched.
#[test]
fn block_on_ligature_lead_recolors_covering_slice() {
    let Some(mut r) = renderer() else {
        eprintln!("SKIP: no ligature test font (set ATERM_FONT or add the repo fixture)");
        return;
    };
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
    // The rest of the arrow (the '>' cell) survives untouched.
    assert_eq!(
        band(&f, 2),
        band(&hidden, 2),
        "the arrow ink outside the cursor rect must survive the lead-cell cursor"
    );
}

/// AUDIT PIN: a block cursor on a CJK (wide) lead inverts BOTH cells as one —
/// the fill spans the glyph's 2-cell footprint — with no erasure anywhere.
/// Pre-W4 the single-cell fill plus the full-glyph bg re-blit erased the
/// ideograph's right half (bg-on-bg in the continuation cell, no cursor there).
#[test]
fn block_on_wide_lead_inverts_both_cells() {
    let Some(mut r) = renderer() else {
        eprintln!("SKIP: no ligature test font (set ATERM_FONT or add the repo fixture)");
        return;
    };
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
    // BOTH cells of the wide glyph carry the cursor fill (the widened rect).
    assert!(
        band(&f, 5).contains(&CURSOR),
        "the wide LEAD cell must show the block cursor fill"
    );
    assert!(
        band(&f, 6).contains(&CURSOR),
        "the wide CONTINUATION cell must show the block cursor fill — the \
         single-cell fill left the right half of the ideograph erased pre-W4"
    );
    // No erasure/bleed outside the 2-cell rect: neighbours byte-identical.
    assert_eq!(band(&f, 4), band(&hidden, 4), "left neighbour untouched");
    assert_eq!(band(&f, 7), band(&hidden, 7), "right neighbour untouched");
}

// ---------------------------------------------------------------------------
// Pure-seam lattice proofs (always-on, no font needed).
// ---------------------------------------------------------------------------

/// EXHAUSTIVE lattice proof of [`clip_span`]'s partition law (the Tier-1
/// binding of the `CursorCutoutClip` ty model): for every glyph extent and
/// window on the lattice, the three sub-window spans tile the extent EXACTLY —
/// every covered x lands in exactly one span — and the middle span never exits
/// the window (no-bleed). Includes the pre-fix NEGATIVE control (an unclipped
/// middle bleeds) and a non-vacuity control (some case has all three slices).
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
                    // Every span stays inside the glyph extent…
                    for s in [left, mid, right].into_iter().flatten() {
                        assert!(s.0 >= gx0 && s.1 <= gx0 + len, "span exits the glyph");
                    }
                    // …and the middle (the cut-out slice) never exits the window.
                    if let Some((mlo, mhi)) = mid {
                        assert!(
                            mlo >= c0.min(c1) && mhi <= c1.max(c0),
                            "cut-out slice bleeds outside the cursor window"
                        );
                    }
                    if left.is_some() && mid.is_some() && right.is_some() {
                        three_way += 1;
                    }
                    // NEGATIVE control: the pre-W4 cut-out used the WHOLE glyph
                    // extent as its "slice". Whenever the window strictly clips,
                    // that violates the no-bleed law this test enforces.
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

/// EXHAUSTIVE lattice proof that [`glyph_quad`]'s x-clip slices the quad at
/// device-pixel x with the matching integer UV shift: for every lattice case,
/// emulated NEAREST sampling of the three slices covers EXACTLY the pixels the
/// unclipped quad covers (disjointly) and samples the IDENTICAL atlas texel at
/// every pixel — quad slicing loses no texels. xs ∈ {1, 2} covers the DECDWL
/// doubled path (half-texel UV starts, same as the proven y-path `v_top`).
#[test]
fn glyph_quad_x_slices_tile_and_preserve_texels() {
    let (aw, ah) = (64.0f32, 64.0f32);
    let (ax, ay, gh, ymin, baseline) = (7u32, 3u32, 4u32, 0i32, 12i32);
    // Emulated fragment NEAREST sample: the atlas texel column addressed at
    // device pixel `x` (integer-aligned rects; fragment centre x+0.5).
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
                            // Tiling: the slices cover exactly the unclipped quad's
                            // pixels, and each samples the identical texel.
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

/// EXHAUSTIVE proof of [`cursor_cutout_cols`] over the complete 2^9 × cc plan
/// lattice: the returned range contains `cc`, is all-`Ligated` exactly when
/// `plan[cc]` is (else it is `{cc}`), and is MAXIMAL (its neighbours are not
/// `Ligated`). This is the source-set policy both renderers share, so proving
/// it here proves them equal by construction.
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
        // Off-plan cursor columns (defensive callers): identity range.
        assert_eq!(cursor_cutout_cols(&plan, N + 3), (N + 3, N + 3));
    }
}
