// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Invariants for the font-metric line decorations: resolved bands stay inside the cell,
//! pattern phase depends only on absolute x (partition invariance), undercurl period and
//! amplitude bounds, and descender ink-skip only erases.
//!
//! ## Proof (the `presentation_gate` idiom)
//!
//! * **Tier-1 (concrete, this file)** — the shipping pattern predicates and
//!   rect emission have small integer domains, so we enumerate a deliberate
//!   lattice (the `procedural_seams` idiom): every partition of a run into
//!   cells produces the IDENTICAL pixel coverage as the whole run
//!   (`pattern_rects_are_partition_invariant`), with non-vacuity controls.
//!
//! The band CLAMP's order (thickness into `[1, cell_h]` first, then the top
//! against `cell_h − t`) is purely additive, so it IS a model:
//! `DecoBandContainment` (`aterm_models_render::deco_band_containment_model`),
//! and `resolved_bands_conform_to_the_deco_band_containment_model` binds the
//! shipping resolver to it over the model's whole lattice. The per-em SCALING
//! and rounding around that clamp (`resolve_deco_metrics`, `undercurl_coverage`)
//! use multiplication, division and f32 rounding, which the ty `Expr` language
//! cannot express (no mul/div — see the box-drawing precedent at
//! `procedural.rs`); per that precedent they are machine-checked here by
//! exhaustive lattice enumeration instead of a model.

use aterm_core::terminal::{Terminal, UnderlineStyle};
use aterm_render::deco::{
    DecoMetrics, DecoTables, intersect_rect_spans, keep_spans_after_ink, resolve_deco_metrics,
};
use aterm_render::{
    ColumnGlyph, Renderer, Theme, overline_rect, strike_rect, undercurl_band, undercurl_coverage,
    undercurl_supported, undercurl_tile_col, underline_band, underline_rects_into,
};

/// The rects one `underline_rects_into` emission pushes, as a fresh `Vec`.
#[allow(clippy::too_many_arguments)]
fn underline_rects(
    style: UnderlineStyle,
    x0: usize,
    y0: usize,
    w: usize,
    cell_w: usize,
    cell_h: usize,
    deco: DecoMetrics,
    curly_mask: bool,
) -> Vec<[usize; 4]> {
    let mut out = Vec::new();
    underline_rects_into(&mut out, style, x0, y0, w, cell_w, cell_h, deco, curly_mask);
    out
}

// Clamping: decoration bands stay inside the cell for every font table.

/// Adversarial lattice for the resolver inputs: every combination must yield
/// bands with `1 <= t <= cell_h` and `y + t <= cell_h`.
#[test]
fn resolved_bands_always_inside_the_cell() {
    let positions = [-4.0f32, -0.5, -0.104, 0.0, 0.05, 0.5, 4.0];
    let thicknesses = [0.001f32, 0.049, 0.1, 0.8, 6.0];
    let pxs = [1.0f32, 13.7, 18.0, 96.0];
    let adjusts = [-64i32, -3, 0, 3, 64];
    let mut checked = 0u64;
    for cell_h in (1..=40).chain([100, 2047, 2048]) {
        for baseline in [
            -8,
            0,
            1,
            cell_h as i32 / 2,
            cell_h as i32,
            cell_h as i32 + 9,
        ] {
            let mut tables = vec![None, Some(DecoTables::default())];
            for &p in &positions {
                for &t in &thicknesses {
                    tables.push(Some(DecoTables {
                        underline: Some((p, t)),
                        strikeout: Some((-p, t)),
                    }));
                }
            }
            for tables in tables {
                for &px in &pxs {
                    for &ap in &adjusts {
                        for &at in &adjusts {
                            let d = resolve_deco_metrics(cell_h, baseline, px, tables, ap, at);
                            for (label, y, t) in [
                                ("underline", d.underline_y, d.underline_t),
                                ("strike", d.strike_y, d.strike_t),
                            ] {
                                assert!(
                                    (1..=cell_h).contains(&t) && y + t <= cell_h,
                                    "{label} band leaves the cell: y={y} t={t} \
                                     cell_h={cell_h} baseline={baseline} px={px} \
                                     tables={tables:?} adjust=({ap},{at})"
                                );
                            }
                            checked += 1;
                        }
                    }
                }
            }
        }
    }
    assert!(checked > 100_000, "lattice must be adversarial ({checked})");

    // Non-finite and extreme inputs, including a zero `cell_h` (treated as 1) and one past
    // the signed range: the casts saturate, and the adjust and clamp arithmetic must be total.
    for cell_h in [0, 1, 27, usize::MAX] {
        for baseline in [i32::MIN, 0, i32::MAX] {
            for px in [f32::NAN, f32::NEG_INFINITY, f32::INFINITY, f32::MAX] {
                for position in [
                    f32::NAN,
                    f32::NEG_INFINITY,
                    f32::INFINITY,
                    -f32::MAX,
                    f32::MAX,
                ] {
                    for thickness in [0.1, f32::INFINITY, f32::MAX] {
                        for adjustment in [i32::MIN, i32::MAX] {
                            let tables = Some(DecoTables {
                                underline: Some((position, thickness)),
                                strikeout: Some((position, thickness)),
                            });
                            let d = resolve_deco_metrics(
                                cell_h, baseline, px, tables, adjustment, adjustment,
                            );
                            for (y, t) in [(d.underline_y, d.underline_t), (d.strike_y, d.strike_t)]
                            {
                                assert!((1..=cell_h.max(1)).contains(&t));
                                assert!(y + t <= cell_h.max(1));
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The table path actually consumes the tables (non-vacuity for the resolver:
/// a synthetic post/OS2 entry moves the band off the heuristic position).
#[test]
fn font_tables_actually_drive_the_bands() {
    let tables = Some(DecoTables {
        underline: Some((-0.2, 0.1)),
        strikeout: Some((0.3, 0.05)),
    });
    let d = resolve_deco_metrics(30, 20, 20.0, tables, 0, 0);
    // Underline: thickness round(0.1·20)=2; center 20+0.2·20=24 → top 23.
    assert_eq!((d.underline_y, d.underline_t), (23, 2));
    // Strike: thickness round(0.05·20)=1; top 20−0.3·20=14.
    assert_eq!((d.strike_y, d.strike_t), (14, 1));
    let legacy = resolve_deco_metrics(30, 20, 20.0, None, 0, 0);
    assert_ne!(
        (d.underline_y, d.underline_t),
        (legacy.underline_y, legacy.underline_t),
        "table-driven band must differ from the heuristic here"
    );
}

/// One settled band of the `DecoBandContainment` lattice through the shipping resolver:
/// `(y, t)` for a `cell_h`-tall cell asked for a raw `(raw_top, raw_t)` band.
///
/// The underline takes the heuristic path (`baseline = 0`, raw band `(1, 1)` for cells of
/// five rows or fewer) and is moved onto the lattice point by the adjust deltas. The strike
/// takes the OS/2 table path at `px = 1`, which lands exactly on the point for any
/// `raw_t >= 1`; a zero-thickness table entry is no entry, so `raw_t = 0` is underline-only.
fn real_bands(cell_h: i64, raw_t: i64, raw_top: i64) -> [Option<(i64, i64)>; 2] {
    let tables = (raw_t >= 1).then_some(DecoTables {
        underline: None,
        strikeout: Some((-(raw_top as f32), raw_t as f32)),
    });
    let d = resolve_deco_metrics(
        cell_h as usize,
        0,
        1.0,
        tables,
        (raw_top - 1) as i32,
        (raw_t - 1) as i32,
    );
    [
        Some((d.underline_y as i64, d.underline_t as i64)),
        tables.map(|_| (d.strike_y as i64, d.strike_t as i64)),
    ]
}

/// Tier-1 conformance for `DecoBandContainment`
/// (`aterm_models_render::deco_band_containment_model`). Over the model's WHOLE
/// lattice — every `cell_h` 1..=5, raw thickness 0..=7 and raw top 0..=7 —
/// the shipping resolver's settled thickness and top must each be exactly the
/// model's one successor of its `SettleThick` and `SettleTop` step, for both
/// the underline and the strikethrough.
///
/// NON-VACUITY: the same lattice must separate the model's two dials — at some
/// points the `Buggy = 1` settle (the top clamped against the whole cell, the
/// thickness floor dropped) differs from the healthy one, and there the real
/// resolver sides with the healthy model. So a resolver regressed either way
/// fails this test at a named point rather than passing it.
#[test]
fn resolved_bands_conform_to_the_deco_band_containment_model() {
    let model = aterm_models_render::deco_band_containment_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let pick = |state: &aterm_spec::interp::State, action: &str, var: &str, value: i64| {
        aterm_spec::interp::pick(&model, state, action, var, value)
            .unwrap_or_else(|| panic!("{action} cannot pick {var} = {value} from {state:?}"))
    };
    let (mut settled, mut separating) = (0usize, 0usize);
    for cell_h in 1..=5 {
        let cell = pick(&model.init_state(), "PickCell", "cell_h", cell_h);
        for raw_t in 0..=7 {
            let thick = pick(&cell, "PickThick", "raw_t", raw_t);
            for raw_top in 0..=7 {
                let raw = pick(&thick, "PickTop", "raw_top", raw_top);
                for (band, real) in ["underline", "strike"]
                    .into_iter()
                    .zip(real_bands(cell_h, raw_t, raw_top))
                {
                    let Some((y, t)) = real else { continue };
                    let at = format!("{band} at cell_h={cell_h} raw_t={raw_t} raw_top={raw_top}");
                    let mut after_thick = raw.clone();
                    after_thick.insert("t", t);
                    after_thick.insert("phase", 4);
                    assert_eq!(
                        model.successors("SettleThick", &raw),
                        vec![after_thick.clone()],
                        "{at}: the real thickness is not the model's SettleThick"
                    );
                    let mut after_top = after_thick.clone();
                    after_top.insert("y", y);
                    after_top.insert("phase", 5);
                    assert_eq!(
                        model.successors("SettleTop", &after_thick),
                        vec![after_top.clone()],
                        "{at}: the real top is not the model's SettleTop"
                    );
                    let pre_fix = buggy
                        .successors("SettleThick", &raw)
                        .into_iter()
                        .flat_map(|s| buggy.successors("SettleTop", &s))
                        .collect::<Vec<_>>();
                    if pre_fix != vec![after_top] {
                        separating += 1;
                    }
                    settled += 1;
                }
            }
        }
    }
    assert_eq!(
        settled,
        5 * 8 * 8 + 5 * 7 * 8,
        "every lattice point, both bands"
    );
    assert!(
        separating > 0,
        "the lattice must reach points where the Buggy = 1 settle differs, or \
         this conformance could not tell a regressed resolver from the shipped one"
    );
}

// ---------------------------------------------------------------------------
// (1) phase continuity — pattern emission is partition-invariant.
// ---------------------------------------------------------------------------

/// Paint the rects of one underline emission into a coverage grid.
fn paint(grid: &mut [Vec<bool>], rects: &[[usize; 4]]) {
    for &[x, y, w, h] in rects {
        for row in grid.iter_mut().skip(y).take(h) {
            row[x..x + w].fill(true);
        }
    }
}

/// For every style, lattice geometry and cell partition of a run, per-cell emission covers
/// the same pixels as whole-run emission: a cell seam cannot reset the pattern phase.
#[test]
fn pattern_rects_are_partition_invariant() {
    let styles = [
        UnderlineStyle::Single,
        UnderlineStyle::Double,
        UnderlineStyle::Curly,
        UnderlineStyle::Dotted,
        UnderlineStyle::Dashed,
    ];
    // Odd/even advances and heights, including 2049 > DECO_ATLAS_MAX_DIM so the Curly
    // square-wave fallback is covered too.
    let cws = [2usize, 3, 7, 8, 11];
    let chs = [3usize, 8, 15, 16, 31, 2049];
    let mut nonvacuous_gaps = 0usize;
    for &cw in &cws {
        for &ch in &chs {
            let dm = resolve_deco_metrics(ch, ch as i32 * 3 / 4, 18.0, None, 0, 0);
            let mask_ok = undercurl_supported(cw, ch);
            for style in styles {
                for ncells in 1..=4usize {
                    for x0 in [0usize, 1, cw + 1] {
                        let w = ncells * cw;
                        let fb_w = x0 + w;
                        // Whole-run emission.
                        let mut whole = vec![vec![false; fb_w]; ch];
                        paint(
                            &mut whole,
                            &underline_rects(style, x0, 0, w, cw, ch, dm, mask_ok),
                        );
                        // Per-cell emission over the same span.
                        let mut cells = vec![vec![false; fb_w]; ch];
                        for c in 0..ncells {
                            paint(
                                &mut cells,
                                &underline_rects(style, x0 + c * cw, 0, cw, cw, ch, dm, mask_ok),
                            );
                        }
                        assert_eq!(
                            whole, cells,
                            "partition changed the pattern: {style:?} cw={cw} ch={ch} \
                             x0={x0} ncells={ncells}"
                        );
                        // Non-vacuity: patterned styles must actually gap.
                        if matches!(style, UnderlineStyle::Dotted | UnderlineStyle::Dashed) {
                            let on = whole.iter().flatten().filter(|&&b| b).count();
                            let band = whole.iter().flatten().count();
                            if on > 0 && on < band {
                                nonvacuous_gaps += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(
        nonvacuous_gaps > 0,
        "dot/dash must produce real gaps somewhere on the lattice"
    );
}

// Undercurl: exact period, amplitude inside the cell band.

/// The tile sampler is periodic in the cell advance and independent of the
/// cell index — the wave cannot reset or jump at a seam, by integer `%`.
#[test]
fn undercurl_sampling_is_periodic_and_seam_independent() {
    for pad in [0usize, 3] {
        for cw in 1..=24usize {
            for rcw in [cw, 2 * cw] {
                for mx in 0..rcw {
                    let base = undercurl_tile_col(pad + mx, pad, rcw, cw);
                    assert!(base < cw, "sampler must stay inside the tile");
                    for c in 1..4usize {
                        assert_eq!(
                            undercurl_tile_col(pad + c * rcw + mx, pad, rcw, cw),
                            base,
                            "period/seam broke: pad={pad} cw={cw} rcw={rcw} c={c} mx={mx}"
                        );
                    }
                }
            }
        }
    }
}

/// Every nonzero coverage byte of the undercurl tile lies within
/// [`undercurl_band`] — the amplitude derivation keeps the whole AA fringe
/// inside the band (and therefore inside the cell) — and that band is the one
/// the ink-skip probe reads ([`underline_band`]) and never starts below the
/// underline. Non-degeneracy: on roomy cells the wave actually waves (crest and
/// trough rows differ).
#[test]
fn undercurl_stays_inside_its_band() {
    let mut waved = 0usize;
    for cw in 1..=24usize {
        for ch in 2..=40usize {
            for tables in [
                None,
                Some(DecoTables {
                    underline: Some((-0.1, 0.05)),
                    strikeout: None,
                }),
            ] {
                for adjust in [-2i32, 0, 2] {
                    let dm = resolve_deco_metrics(ch, ch as i32 * 3 / 4, 18.0, tables, adjust, 0);
                    let mask = undercurl_coverage(cw, ch, dm);
                    assert_eq!(mask.len(), cw * ch);
                    let (top, bottom) = undercurl_band(ch, dm);
                    assert!(top <= dm.underline_y && bottom == ch, "{dm:?}");
                    assert_eq!(
                        underline_band(UnderlineStyle::Curly, ch, dm),
                        Some((top, bottom))
                    );
                    let mut total = 0u32;
                    for y in 0..ch {
                        for x in 0..cw {
                            let v = mask[y * cw + x];
                            total += u32::from(v);
                            assert!(
                                v == 0 || (top..bottom).contains(&y),
                                "coverage escaped the band: cw={cw} ch={ch} \
                                 band=[{top}, {bottom}) at ({x},{y})"
                            );
                        }
                    }
                    assert!(
                        total > 0,
                        "the undercurl must draw something: cw={cw} ch={ch}"
                    );
                    // Crest vs trough: the stroke centre moves across the tile.
                    if cw >= 8 && ch >= 16 {
                        let centroid = |x: usize| -> f64 {
                            let (mut num, mut den) = (0f64, 0f64);
                            for y in 0..ch {
                                let v = f64::from(mask[y * cw + x]);
                                num += v * y as f64;
                                den += v;
                            }
                            num / den.max(1.0)
                        };
                        if (centroid(0) - centroid(cw / 2)).abs() > 1.0 {
                            waved += 1;
                        }
                    }
                }
            }
        }
    }
    assert!(
        waved > 0,
        "the cosine must produce a visible wave somewhere"
    );
}

/// A curly underline WAVES however close to the cell bottom the font puts its
/// underline. Ubuntu Mono's `post` underline (-0.138 em, 0.079 em thick) over a
/// cell whose baseline leaves three rows below it drew a one-row band — a
/// straight line, the same as a single underline — at every size from 10 to
/// 32 px. Every cell with room for `t + 3` rows must swing its stroke centre by
/// at least one pixel between the tile's edge and its middle.
#[test]
fn undercurl_waves_however_low_the_font_puts_its_underline() {
    let ubuntu_mono = DecoTables {
        underline: Some((-0.138, 0.079)),
        strikeout: None,
    };
    let centroid = |mask: &[u8], cw: usize, ch: usize, x: usize| -> f64 {
        let (mut num, mut den) = (0f64, 0f64);
        for y in 0..ch {
            let v = f64::from(mask[y * cw + x]);
            num += v * y as f64;
            den += v;
        }
        num / den.max(1.0)
    };
    for ch in 8..=48usize {
        for px in [ch as f32 * 0.85, ch as f32 * 0.9] {
            for below in [1i32, 2, 3] {
                let dm = resolve_deco_metrics(ch, ch as i32 - below, px, Some(ubuntu_mono), 0, 0);
                if ch < dm.underline_t + 3 {
                    continue;
                }
                let cw = (ch / 2).max(4);
                let mask = undercurl_coverage(cw, ch, dm);
                let swing = (centroid(&mask, cw, ch, 0) - centroid(&mask, cw, ch, cw / 2)).abs();
                assert!(
                    swing >= 1.0,
                    "ch={ch} px={px} baseline={} {dm:?}: the curl swings {swing:.2} px — \
                     it draws as a straight underline",
                    ch as i32 - below
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// (3) ink-skip — coverage-monotone, no-ink identity, dilation exactness.
// ---------------------------------------------------------------------------

/// Exhaustive over every ink mask up to 12 columns: the kept spans are exactly
/// the columns with no ink within 1px — a pure erase (never adds), identity
/// when inkless.
#[test]
fn ink_skip_spans_exhaustive() {
    for w in 0..=12usize {
        for bits in 0u32..(1 << w) {
            let ink: Vec<bool> = (0..w).map(|i| bits & (1 << i) != 0).collect();
            let mut spans = Vec::new();
            keep_spans_after_ink(&ink, &mut spans);
            let mut kept = vec![false; w];
            for &(s, l) in &spans {
                assert!(l > 0 && s + l <= w, "span leaves the cell: {s}+{l} > {w}");
                for k in kept.iter_mut().skip(s).take(l) {
                    assert!(!*k, "spans must not overlap");
                    *k = true;
                }
            }
            for i in 0..w {
                let dilated = ink[i] || (i > 0 && ink[i - 1]) || (i + 1 < w && ink[i + 1]);
                assert_eq!(
                    kept[i], !dilated,
                    "dilation exactness broke at col {i} of {ink:?}"
                );
            }
            if bits == 0 {
                let full: Vec<bool> = vec![true; w];
                assert_eq!(kept, full, "no ink must keep the whole span (identity)");
            }
        }
    }
}

/// `intersect_rect_spans` output coverage == rect ∩ spans (exhaustive small
/// lattice) — the rect subtraction can only erase.
#[test]
fn intersect_rect_spans_is_exact() {
    for rx in 0..6usize {
        for rw in 0..6usize {
            for s0 in 0..6usize {
                for l0 in 0..4usize {
                    for s1 in 6..9usize {
                        for l1 in 0..4usize {
                            let spans = [(s0, l0), (s1, l1)];
                            let mut out = Vec::new();
                            intersect_rect_spans(&mut out, [rx, 2, rw, 3], &spans);
                            let mut got = vec![false; 16];
                            for &[x, y, w, h] in &out {
                                assert_eq!((y, h), (2, 3), "y-extent must be untouched");
                                got[x..x + w].fill(true);
                            }
                            let want: Vec<bool> = (0..16)
                                .map(|x| {
                                    (rx..rx + rw).contains(&x)
                                        && spans.iter().any(|&(s, l)| (s..s + l).contains(&x))
                                })
                                .collect();
                            assert_eq!(got, want, "rect={rx}+{rw} spans={spans:?}");
                        }
                    }
                }
            }
        }
    }
}

// Ink-skip end to end through the real renderer: monotone per pixel, byte-identical for
// descender-free cells.

/// The tracked fixture face at 18px; a missing file or failed build fails the test.
fn fixture_renderer() -> Renderer {
    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/jetbrains-mono.ttf"
    );
    let bytes = std::fs::read(FIXTURE).expect("the tracked fixture font must be readable");
    Renderer::from_bytes(&bytes, 18.0, Theme::default())
        .expect("the tracked fixture font must build an 18px renderer")
}

fn render(r: &mut Renderer, bytes: &[u8]) -> aterm_render::Frame {
    let mut term = Terminal::new(2, 8);
    term.process(bytes);
    let input = term.cell_frame(2, 8);
    r.render_input(&input)
}

/// Pixel-level coverage monotonicity: with underline+skip (B), underline without skip (A)
/// and no underline (C), every pixel of B equals A or C. Descenders (gy) must erase
/// something; a descender-free run (--) is byte-identical with the knob on or off.
#[test]
fn ink_skip_is_coverage_monotone_end_to_end() {
    let mut r = fixture_renderer();
    assert!(r.underline_skip_descenders(), "W7 ships DEFAULT ON");

    // Descender text, underlined: A (skip off) / B (skip on) / C (no underline).
    let deco_txt = b"\x1b[4mgy\x1b[0m";
    let plain_txt = b"gy";
    r.set_underline_skip_descenders(false);
    let a = render(&mut r, deco_txt);
    r.set_underline_skip_descenders(true);
    let b = render(&mut r, deco_txt);
    let c = render(&mut r, plain_txt);
    assert_eq!(a.pixels.len(), b.pixels.len());
    assert_eq!(b.pixels.len(), c.pixels.len());
    let mut erased = 0usize;
    for i in 0..b.pixels.len() {
        assert!(
            b.pixels[i] == a.pixels[i] || b.pixels[i] == c.pixels[i],
            "ink-skip invented a pixel at {i}: skip-on {:#010x} vs \
             skip-off {:#010x} / no-underline {:#010x}",
            b.pixels[i],
            a.pixels[i],
            c.pixels[i]
        );
        if b.pixels[i] != a.pixels[i] {
            erased += 1;
        }
    }
    assert!(
        erased > 0,
        "gy descenders must actually erase some underline coverage (non-vacuity)"
    );

    // Descender-free text: skip on == skip off, byte for byte.
    let flat_txt = b"\x1b[4m--\x1b[0m";
    r.set_underline_skip_descenders(false);
    let off = render(&mut r, flat_txt);
    r.set_underline_skip_descenders(true);
    let on = render(&mut r, flat_txt);
    assert_eq!(
        off.pixels, on.pixels,
        "a cell with no descender ink must render byte-identically"
    );
}

/// Baseline bottoms are not descenders: at 12px the underline band starts within 1px of
/// the baseline, so the ink probe is clamped strictly below the baseline row. Round
/// bottoms (a/e/u) overshoot onto the baseline row by design and must not chop the line;
/// the 18px fixture above leaves a row of air and cannot catch this.
#[test]
fn baseline_bottoms_keep_the_underline_at_12px() {
    let bytes = embedded_font_bytes();
    let mut r = Renderer::from_bytes(&bytes, 12.0, Theme::default())
        .expect("the embedded font must build a 12px renderer");
    // Descender-free letters, underlined: skip on == skip off, byte for byte —
    // the underline must survive continuously under baseline-sitting bottoms.
    let flat_txt = b"\x1b[4mnnaeuu\x1b[0m";
    r.set_underline_skip_descenders(false);
    let off = render(&mut r, flat_txt);
    r.set_underline_skip_descenders(true);
    let on = render(&mut r, flat_txt);
    assert_eq!(
        off.pixels, on.pixels,
        "baseline-sitting letter bottoms must not chop the 12px underline"
    );
    // Control (non-vacuity at this size): real descenders still erase.
    let deco_txt = b"\x1b[4mgyjpqy\x1b[0m";
    r.set_underline_skip_descenders(false);
    let a = render(&mut r, deco_txt);
    r.set_underline_skip_descenders(true);
    let b = render(&mut r, deco_txt);
    assert_ne!(
        a.pixels, b.pixels,
        "g/y descenders must still erase underline coverage at 12px"
    );
}

/// The embedded DejaVu Sans Mono, from the checked-in asset whatever features
/// the build turned on.
fn embedded_font_bytes() -> Vec<u8> {
    include_bytes!("../assets/DejaVuSansMono.ttf").to_vec()
}

/// The undercurl band derivation agrees with the resolved metrics for every
/// style, and `Curly` emits NO rects on mask-supported sizes (the tile draws
/// instead) but real square-wave rects when the mask is unsupported.
#[test]
fn curly_gates_on_the_shared_mask_predicate() {
    let dm = DecoMetrics {
        underline_y: 20,
        underline_t: 2,
        strike_y: 8,
        strike_t: 2,
    };
    assert!(undercurl_supported(9, 24));
    assert!(
        underline_rects(UnderlineStyle::Curly, 0, 0, 18, 9, 24, dm, true).is_empty(),
        "mask-supported curly draws via the tile, not rects"
    );
    assert!(
        !underline_rects(UnderlineStyle::Curly, 0, 0, 18, 9, 24, dm, false).is_empty(),
        "mask-unsupported curly falls back to square-wave rects"
    );
    // The shared predicate: 8 sprites must fit 2048 texels (256 is the widest base cell).
    assert!(undercurl_supported(256, 24), "8·256 ≤ 2048");
    assert!(!undercurl_supported(257, 24), "8·257 > 2048");
    assert!(!undercurl_supported(9, 2049), "height cap");
    assert!(
        !undercurl_supported(usize::MAX, 24),
        "oversized width is refused without overflow"
    );
    assert!(
        !undercurl_supported(9, usize::MAX),
        "oversized height is refused"
    );
    // Band derivation across styles.
    assert_eq!(underline_band(UnderlineStyle::None, 24, dm), None);
    assert_eq!(
        underline_band(UnderlineStyle::Single, 24, dm),
        Some((20, 22))
    );
    // The curl's band runs to the cell bottom and is at least `t + 3` rows
    // tall, so this 4-row gap under a 2px stroke rises one row.
    assert_eq!(
        underline_band(UnderlineStyle::Curly, 24, dm),
        Some((19, 24))
    );
    assert_eq!(
        underline_band(UnderlineStyle::Double, 24, dm),
        Some((16, 22)),
        "double band spans both rails"
    );
    let oversized = DecoMetrics {
        underline_y: 0,
        underline_t: usize::MAX,
        ..dm
    };
    assert_eq!(
        underline_band(UnderlineStyle::Double, usize::MAX, oversized),
        Some((0, usize::MAX)),
        "an oversized double-rail gap clamps to the cell"
    );
}

// Decoration never clips: every rect `underline_rects_into`, `strike_rect` and
// `overline_rect` emit stays within `[x0, x0+w) × [y0, y0+cell_h)` for any style and any
// DecoMetrics, including out-of-range ones the emitters re-clamp. The raster and GPU loops
// blit without a per-rect bounds check on the strength of this.

/// Exhaustive over styles × cell geometry × run placement × adversarial metrics: every
/// emitted rect is contained in its run band on both axes.
#[test]
fn decoration_writes_stay_within_the_run_band() {
    let styles = [
        UnderlineStyle::None,
        UnderlineStyle::Single,
        UnderlineStyle::Double,
        UnderlineStyle::Curly,
        UnderlineStyle::Dotted,
        UnderlineStyle::Dashed,
    ];
    // Legitimate bands and rogue ones that would escape the cell.
    let metrics = |cell_h: usize| {
        [
            DecoMetrics {
                underline_y: 0,
                underline_t: 1,
                strike_y: 0,
                strike_t: 1,
            },
            DecoMetrics {
                underline_y: cell_h.saturating_sub(1),
                underline_t: 1,
                strike_y: cell_h / 2,
                strike_t: 1,
            },
            // Rogue: top below the floor and a thickness far past the cell.
            DecoMetrics {
                underline_y: cell_h + 50,
                underline_t: cell_h + 99,
                strike_y: cell_h + 7,
                strike_t: cell_h + 40,
            },
        ]
    };

    // Non-vacuity per style, indexed by `style_idx`.
    let mut style_emitted = [false; 6];
    let mut top_reached = false; // Double upper rail / overline hug the top
    let mut bottom_reached = false; // Curly / low underline hug the bottom
    let mut checked = 0u64;

    for cell_h in [1usize, 2, 3, 7, 12, 13, 20, 24, 40] {
        for cell_w in [1usize, 2, 6, 7, 9, 12] {
            for &x0 in &[0usize, 5, 130] {
                for &y0 in &[0usize, 3, 900] {
                    for w in [cell_w, 2 * cell_w, 5 * cell_w] {
                        for dm in metrics(cell_h) {
                            for &style in &styles {
                                for curly_mask in [false, true] {
                                    let rects = underline_rects(
                                        style, x0, y0, w, cell_w, cell_h, dm, curly_mask,
                                    );
                                    for r in &rects {
                                        assert_contained(*r, x0, y0, w, cell_h, style, curly_mask);
                                        if r[1] == y0 {
                                            top_reached = true;
                                        }
                                        if r[1] + r[3] == y0 + cell_h {
                                            bottom_reached = true;
                                        }
                                    }
                                    if !rects.is_empty() {
                                        style_emitted[style_idx(style)] = true;
                                    }
                                    checked += 1;
                                }
                                // Strike + overline share the same containment law.
                                for r in [
                                    strike_rect(x0, y0, w, cell_h, dm),
                                    overline_rect(x0, y0, w, cell_h, dm),
                                ]
                                .into_iter()
                                .flatten()
                                {
                                    assert_contained(
                                        r,
                                        x0,
                                        y0,
                                        w,
                                        cell_h,
                                        UnderlineStyle::None,
                                        false,
                                    );
                                    if r[1] == y0 {
                                        top_reached = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Non-vacuity: every ink-emitting style emitted rects, and rects reached the cell top
    // and bottom.
    for s in [
        UnderlineStyle::Single,
        UnderlineStyle::Double,
        UnderlineStyle::Curly,
        UnderlineStyle::Dotted,
        UnderlineStyle::Dashed,
    ] {
        assert!(
            style_emitted[style_idx(s)],
            "style {s:?} never emitted a rect to check"
        );
    }
    assert!(
        top_reached,
        "non-vacuity: no rect ever reached the cell top"
    );
    assert!(
        bottom_reached,
        "non-vacuity: no rect ever reached the cell bottom"
    );
    assert!(checked > 10_000, "lattice must be dense ({checked})");
}

/// Dense index for an `UnderlineStyle`, which derives neither `Ord` nor `Hash`.
fn style_idx(s: UnderlineStyle) -> usize {
    match s {
        UnderlineStyle::None => 0,
        UnderlineStyle::Single => 1,
        UnderlineStyle::Double => 2,
        UnderlineStyle::Curly => 3,
        UnderlineStyle::Dotted => 4,
        UnderlineStyle::Dashed => 5,
    }
}

/// A rect `[rx, ry, rw, rh]` must lie inside `[x0, x0+w) × [y0, y0+cell_h)`.
fn assert_contained(
    r: [usize; 4],
    x0: usize,
    y0: usize,
    w: usize,
    cell_h: usize,
    style: UnderlineStyle,
    curly_mask: bool,
) {
    let [rx, ry, rw, rh] = r;
    assert!(
        rx >= x0 && rx + rw <= x0 + w,
        "decoration escaped its run horizontally: rect={r:?} run x=[{x0},{}) \
         style={style:?} curly_mask={curly_mask}",
        x0 + w
    );
    assert!(
        ry >= y0 && ry + rh <= y0 + cell_h,
        "decoration escaped its cell vertically: rect={r:?} band y=[{y0},{}) \
         style={style:?} curly_mask={curly_mask}",
        y0 + cell_h
    );
}

// ---------------------------------------------------------------------------
// (5) the CJK composition underline — a wide glyph's rule is never carved.
// ---------------------------------------------------------------------------

/// Render `text` as an IME preedit at column 2 of row 0, as
/// `RenderInput::overlay_ime_preedit` paints a live composition. The `$ ` prompt matters:
/// a pristine `Terminal::new` resolves no default colours, so on an empty row the overlay
/// seeds from `RenderCell::default()` (black on black) and every pixel assertion would
/// pass vacuously.
fn preedit_frame(r: &mut Renderer, text: &str) -> aterm_render::Frame {
    let mut term = Terminal::new(2, 12);
    term.process(b"$ ");
    let mut input = term.cell_frame(2, 12);
    input.overlay_ime_preedit(text, None, false);
    r.render_input(&input)
}

/// The composition underline under a wide glyph is a continuous rule, not dashes.
///
/// Composed cells are `UnderlineStyle::Single`, so the rule goes through descender ink-skip,
/// whose probe cannot tell a Han ideograph's AA-overshooting foot from a descender tail;
/// wide cells are therefore exempt. Asserted as policy (the wide lead takes the no-skip
/// path) and in pixels (byte-identical with skip on and off, no background gap in the rule).
/// `cell.wide` comes from East Asian Width, so the policy half needs no CJK face.
#[test]
fn cjk_composition_underline_is_continuous() {
    let bytes = embedded_font_bytes();
    // 12px is the Latin ink-skip's tuning size; the rest cover common text sizes.
    for px in [12.0f32, 13.0, 14.0, 16.0, 18.0, 24.0, 32.0] {
        let mut r = Renderer::from_bytes(&bytes, px, Theme::default())
            .unwrap_or_else(|e| panic!("the embedded font must build a {px}px renderer: {e}"));
        assert!(r.underline_skip_descenders(), "W7 ships DEFAULT ON");
        let (cw, _) = r.cell_size();
        let pad = r.pad();

        // Policy: the wide lead is exempt, so its rule is drawn whole.
        let mut term = Terminal::new(2, 12);
        term.process(b"$ ");
        let mut input = term.cell_frame(2, 12);
        input.overlay_ime_preedit("日本", None, false);
        let (mut ink, mut spans) = (Vec::new(), Vec::new());
        let carved = r.underline_keep_spans_into(
            &input,
            0,
            2,
            ColumnGlyph::PerCell,
            pad + 2 * cw,
            2 * cw,
            &mut ink,
            &mut spans,
        );
        assert!(
            !carved,
            "{px}px: the lead cell of a composed 日 must take the UNSKIPPED draw \
             — an ideograph's foot is not a descender. Got carved spans {spans:?}"
        );

        // Pixels: identical with the feature on and off.
        r.set_underline_skip_descenders(false);
        let off = preedit_frame(&mut r, "日本");
        r.set_underline_skip_descenders(true);
        let on = preedit_frame(&mut r, "日本");
        assert_eq!(
            off.pixels, on.pixels,
            "{px}px: a composed CJK run must render byte-identically with \
             descender ink-skip on or off"
        );

        // Pixels: no background gap in the rule across the composed span.
        let dm = r.deco_metrics();
        let w = on.width;
        let bg = on.pixels[0];
        let (x0, x1) = (pad + 2 * cw, (pad + 6 * cw).min(w));
        for y in dm.underline_y..(dm.underline_y + dm.underline_t).min(on.height) {
            let row = &on.pixels[y * w..y * w + w];
            let first = (x0..x1).find(|&x| row[x] != bg);
            let last = (x0..x1).rev().find(|&x| row[x] != bg);
            let (Some(first), Some(last)) = (first, last) else {
                continue;
            };
            let gaps: Vec<usize> = (first..=last).filter(|&x| row[x] == bg).collect();
            assert!(
                gaps.is_empty(),
                "{px}px: the composition rule is broken at row {y}, columns \
                 {gaps:?} of the composed span {x0}..{x1} fell back to \
                 background — that is the dashes-under-每-character defect"
            );
        }
    }
}
