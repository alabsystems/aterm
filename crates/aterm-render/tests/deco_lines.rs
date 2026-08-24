// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! W7 machine-checked invariants for the font-metric line decorations:
//! resolved-band in-cell clamping, absolute-x pattern phase continuity
//! (partition invariance), undercurl period exactness + amplitude bounds, and
//! descender ink-skip coverage-monotonicity.
//!
//! ## Two-tier proof (the `presentation_gate` idiom)
//!
//! * **Tier-0 (abstract, model-checked by the Trust `ty` compiler)** — the
//!   `DecoPhase` derived model (`aterm_spec::derive::deco_phase_model`)
//!   carries the `PhasePure` invariant: the pattern phase counter equals a
//!   ghost counter driven ONLY by absolute x, across arbitrarily placed cell
//!   seams. `cargo test -p aterm-spec`
//!   (`derived_deco_phase_proves_and_catches_seam_reset`) runs the REAL `ty`
//!   binary: it PROVES the invariant at `Buggy=0` and CATCHES the historical
//!   per-cell phase restart at `Buggy=1` (counterexample required).
//! * **Tier-1 (concrete, this file)** — the shipping pattern predicates and
//!   rect emission have small integer domains, so we enumerate a deliberate
//!   lattice (the `procedural_seams` idiom): every partition of a run into
//!   cells produces the IDENTICAL pixel coverage as the whole run
//!   (`pattern_rects_are_partition_invariant`), with non-vacuity controls.
//!
//! The rounding/clamping laws (`resolve_deco_metrics`, `undercurl_coverage`)
//! use integer division and f32 rounding, which the ty `Expr` language cannot
//! express (no mul/div — see the box-drawing precedent at
//! `procedural.rs`); per that precedent they are machine-checked here by
//! exhaustive lattice enumeration instead of a model.

use aterm_core::terminal::{Terminal, UnderlineStyle};
use aterm_render::deco::{
    DecoMetrics, DecoTables, intersect_rect_spans, keep_spans_after_ink, pattern_spans_into,
    resolve_deco_metrics,
};
use aterm_render::{
    Renderer, Theme, strike_overline_rects, undercurl_coverage, undercurl_supported,
    undercurl_tile_col, underline_band, underline_rects,
};

// ---------------------------------------------------------------------------
// (4) clamping — decoration bands always inside the cell, for ALL font tables.
// ---------------------------------------------------------------------------

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
}

/// Regression pin: with NO tables and NO adjusts, the resolver reproduces the
/// pre-W7 hardcoded bands byte-for-byte (`cell_h/15` at `baseline + t`, strike
/// a third of the ascent above the baseline).
#[test]
fn legacy_fallback_matches_pre_w7_bands() {
    for cell_h in 1..=120usize {
        for baseline in [-3i32, 0, 2, cell_h as i32 * 3 / 4, cell_h as i32 + 2] {
            let d = resolve_deco_metrics(cell_h, baseline, 18.0, None, 0, 0);
            let t = (cell_h / 15).max(1);
            let base = baseline.max(0) as usize;
            let uy_old = (base + t).min(cell_h.saturating_sub(t));
            let sy_old = base
                .saturating_sub((base / 3).max(1))
                .min(cell_h.saturating_sub(t));
            assert_eq!(
                (d.underline_y, d.underline_t, d.strike_y, d.strike_t),
                (uy_old, t.min(cell_h), sy_old, t.min(cell_h)),
                "legacy band drifted at cell_h={cell_h} baseline={baseline}"
            );
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

// ---------------------------------------------------------------------------
// (1) phase continuity — pattern emission is partition-invariant (Tier-1 of
//     the `deco_phase` ty model).
// ---------------------------------------------------------------------------

/// Paint the rects of one `underline_rects` emission into a coverage grid.
fn paint(grid: &mut [Vec<bool>], rects: &[[usize; 4]]) {
    for &[x, y, w, h] in rects {
        for row in grid.iter_mut().skip(y).take(h) {
            row[x..x + w].fill(true);
        }
    }
}

/// For EVERY style, every lattice geometry, and every cell partition of a run,
/// per-cell emission covers the IDENTICAL pixel set as whole-run emission —
/// the pattern's value at a pixel is a pure function of absolute x, so a cell
/// seam cannot reset it. (The historical code restarted dash/dot/wave phase at
/// every cell: the negative control asserts the old per-cell dash phasing
/// actually violates this.)
#[test]
fn pattern_rects_are_partition_invariant() {
    let styles = [
        UnderlineStyle::Single,
        UnderlineStyle::Double,
        UnderlineStyle::Curly,
        UnderlineStyle::Dotted,
        UnderlineStyle::Dashed,
    ];
    // Odd/even cell advances + heights (the procedural_seams lattice idiom),
    // including a mask-unsupported height (2049 > DECO_ATLAS_MAX_DIM) so the
    // Curly SQUARE-WAVE fallback is exercised through the same theorem.
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

/// NEGATIVE CONTROL: the historical per-cell dash phasing (`dash = w/3`,
/// restarting at each cell origin) is NOT partition-invariant — the theorem
/// above genuinely rules the old behavior out.
#[test]
fn old_per_cell_dash_phasing_fails_partition_invariance() {
    // The pre-W7 dashed emission, verbatim.
    let old_dashed = |x0: usize, w: usize| -> Vec<(usize, usize)> {
        let dash = (w / 3).max(1);
        let step = dash + (dash / 2).max(1);
        let mut out = Vec::new();
        let mut x = x0;
        while x < x0 + w {
            out.push((x, dash.min(x0 + w - x)));
            x += step;
        }
        out
    };
    let cw = 9usize;
    let cover = |spans: &[(usize, usize)], fb: usize| -> Vec<bool> {
        let mut g = vec![false; fb];
        for &(x, w) in spans {
            g[x..x + w].fill(true);
        }
        g
    };
    let whole = cover(&old_dashed(0, 2 * cw), 2 * cw);
    let mut split = old_dashed(0, cw);
    split.extend(old_dashed(cw, cw));
    let split = cover(&split, 2 * cw);
    assert_ne!(
        whole, split,
        "the old dash law was partition-dependent; if this ever passes, the \
         negative control is dead"
    );
}

// ---------------------------------------------------------------------------
// (2) undercurl — period exactness + amplitude bounds within the cell band.
// ---------------------------------------------------------------------------

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

/// Every nonzero coverage byte of the undercurl tile lies within the
/// `[underline_y, cell_h)` band — the amplitude derivation keeps the whole AA
/// fringe inside the band (and therefore inside the cell). Non-degeneracy: on
/// roomy cells the wave actually waves (crest and trough rows differ).
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
                    let mut total = 0u32;
                    for y in 0..ch {
                        for x in 0..cw {
                            let v = mask[y * cw + x];
                            total += u32::from(v);
                            assert!(
                                v == 0 || (dm.underline_y..ch).contains(&y),
                                "coverage escaped the band: cw={cw} ch={ch} \
                                 band=[{}, {ch}) at ({x},{y})",
                                dm.underline_y
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

// ---------------------------------------------------------------------------
// (3, end-to-end) ink-skip through the REAL renderer: monotone at the pixel
// level, byte-identical for descender-free cells.
// ---------------------------------------------------------------------------

fn fixture_renderer() -> Option<Renderer> {
    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/jetbrains-mono.ttf"
    );
    let bytes = std::fs::read(FIXTURE).ok()?;
    Renderer::from_bytes(&bytes, 18.0, Theme::default()).ok()
}

fn render(r: &mut Renderer, bytes: &[u8]) -> aterm_render::Frame {
    let mut term = Terminal::new(2, 8);
    term.process(bytes);
    let input = term.cell_frame(2, 8);
    r.render_input(&input)
}

/// The coverage-monotone theorem at the pixel level: with underline+skip (B),
/// underline without skip (A), and no underline (C) over the same text, every
/// pixel of B equals A (underline kept) or C (underline erased) — ink-skip can
/// only ZERO underline coverage, never invent pixels. Descenders (gy) force a
/// real erase (non-vacuity); a descender-free cell (--) is BYTE-IDENTICAL with
/// the knob on or off (the no-ink identity, i.e. the same code path as off).
#[test]
fn ink_skip_is_coverage_monotone_end_to_end() {
    let Some(mut r) = fixture_renderer() else {
        eprintln!("SKIP: missing test fixture font");
        return;
    };
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

/// Baseline bottoms are NOT descenders (TYPOGRAPHY R2): at the live desktop
/// 12px the resolved underline band starts within 1px of the baseline, and the
/// probe's 1px vertical dilation used to reach the bottom row of EVERY letter
/// — chopping a continuous underline (and the tab strip's accent rule, which
/// is drawn as a cell underline) into dashes. The probe is now clamped to
/// strictly below the baseline row: only true descender ink skips. Strictly,
/// because round bottoms (a/e/u here — this is why the text is `nnaeuu`, not
/// just flat-footed letters) OVERSHOOT the baseline by design, and at 12px
/// that lands one AA row exactly ON the baseline row — sitting on the line,
/// not descending through it. Guarded HERE at the tight geometry — the 18px
/// fixture of the test above leaves a row of air under the letters, so it
/// never caught this.
#[test]
fn baseline_bottoms_keep_the_underline_at_12px() {
    let Some(bytes) = embedded_font_bytes() else {
        eprintln!("SKIP: embedded font unavailable");
        return;
    };
    let Ok(mut r) = Renderer::from_bytes(&bytes, 12.0, Theme::default()) else {
        eprintln!("SKIP: 12px renderer failed to build");
        return;
    };
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

/// The embedded DejaVu Sans Mono, via the public accessor when the (default)
/// `embedded-font` feature is on; `None` (skip) on a --no-default-features run.
fn embedded_font_bytes() -> Option<Vec<u8>> {
    #[cfg(feature = "embedded-font")]
    {
        Some(aterm_render::embedded_font().to_vec())
    }
    #[cfg(not(feature = "embedded-font"))]
    {
        None
    }
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
    // The shared predicate itself: 9 sprites must fit 2048 texels
    // (⌊2048/9⌋ = 227 is the widest supported base cell).
    assert!(undercurl_supported(227, 24), "9·227 ≤ 2048");
    assert!(!undercurl_supported(228, 24), "9·228 > 2048");
    assert!(!undercurl_supported(9, 2049), "height cap");
    // Band derivation across styles.
    assert_eq!(underline_band(UnderlineStyle::None, 24, dm), None);
    assert_eq!(
        underline_band(UnderlineStyle::Single, 24, dm),
        Some((20, 22))
    );
    assert_eq!(
        underline_band(UnderlineStyle::Curly, 24, dm),
        Some((20, 24))
    );
    assert_eq!(
        underline_band(UnderlineStyle::Double, 24, dm),
        Some((16, 22)),
        "double band spans both rails"
    );
}

// ---------------------------------------------------------------------------
// CROSS-CUTTING THEOREM (c) — DECORATION NEVER CLIPS.
//
// Every rectangle any decoration WRITE PATH emits — the underline emitter
// (`underline_rects`, all six `UnderlineStyle`s incl. the AA-masked and legacy
// square-wave curl) and the strike/overline emitter (`strike_overline_rects`) —
// stays entirely inside its run's band: horizontally within `[x0, x0+w)` and
// vertically within `[y0, y0+cell_h)`. No decoration ever bleeds into the row
// above/below or past the drawn run, for ANY style, ANY (integer) DecoMetrics —
// including hand-built OUT-OF-RANGE metrics, which the emitters re-clamp.
//
// `resolved_bands_always_inside_the_cell` (above) proves the RESOLVER produces
// in-cell metrics; THIS proves the downstream EMITTERS never escape even if fed
// a rogue band — the containment the raster/GPU loops rely on to blit without a
// per-rect bounds check. It is the theorem no single style owns.
// ---------------------------------------------------------------------------

/// THE THEOREM: exhaustive over styles × cell geometry × run placement ×
/// adversarial (incl. out-of-range) DecoMetrics, every emitted rect is contained
/// in its run band on both axes.
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
    // Adversarial metrics: legitimate bands AND rogue ones (y/thickness that
    // WOULD escape the cell) — the emitters must clamp every one back in.
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
            // ROGUE: top at/below the floor and a thickness far past the cell.
            DecoMetrics {
                underline_y: cell_h + 50,
                underline_t: cell_h + 99,
                strike_y: cell_h + 7,
                strike_t: cell_h + 40,
            },
        ]
    };

    // non-vacuity per style, indexed by `style_idx` (UnderlineStyle isn't Ord/Hash).
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
                                for (st, ov) in [(true, false), (false, true), (true, true)] {
                                    let rects =
                                        strike_overline_rects(st, ov, x0, y0, w, cell_h, dm);
                                    for r in &rects {
                                        assert_contained(
                                            *r,
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
    }

    // NON-VACUITY: every ink-emitting style genuinely produced rects (so the
    // containment check was not vacuous for any of them), and rects that hug the
    // cell TOP and cell BOTTOM were both exercised — the two extremes clipping
    // would violate first.
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

/// Dense index for an `UnderlineStyle` (it derives neither `Ord` nor `Hash`),
/// for the per-style non-vacuity bookkeeping.
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

/// NEGATIVE CONTROL — the containment predicate genuinely REJECTS an out-of-band
/// rect, so a passing theorem above is not a tautology (a rect one px below the
/// cell must fail `assert_contained`).
#[test]
#[should_panic(expected = "escaped its cell vertically")]
fn out_of_band_rect_is_rejected() {
    // A rect whose bottom is one px past the cell — exactly what a pre-clamp
    // emitter bug would produce.
    assert_contained([0, 0, 4, 25], 0, 0, 4, 24, UnderlineStyle::Single, false);
}

/// `pattern_spans_into` (the public span twin of the rect emitter) is also
/// partition-invariant — it feeds the ink-skip interval algebra.
#[test]
fn pattern_spans_partition_invariant() {
    let on = |x: usize| aterm_render::deco::dashed_on(x, 8);
    let mut whole = Vec::new();
    pattern_spans_into(&mut whole, 3, 32, on);
    let mut parts = Vec::new();
    for c in 0..4 {
        let mut p = Vec::new();
        pattern_spans_into(&mut p, 3 + c * 8, 8, on);
        parts.extend(p);
    }
    let cover = |spans: &[(usize, usize)]| -> Vec<bool> {
        let mut g = vec![false; 64];
        for &(x, w) in spans {
            g[x..x + w].fill(true);
        }
        g
    };
    assert_eq!(cover(&whole), cover(&parts));
}
