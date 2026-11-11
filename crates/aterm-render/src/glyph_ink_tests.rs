// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A glyph whose pixels all sit at coverage `a`.
fn flat(a: u8, n: u16) -> GlyphCoverage {
    let mut c = GlyphCoverage {
        max: a,
        ..GlyphCoverage::default()
    };
    c.levels[usize::from(a)] = n;
    c
}

fn table(glyph: GlyphCoverage, corrected: bool) -> GlyphInk {
    GlyphInk::from_coverage([glyph; ASCII], [glyph; ASCII], corrected)
}

/// The core of a glyph drawn at full coverage is its colour whole, and a thin one's lies
/// lower; with the remap, dark ink on paper covers more than its raw coverage, light ink on
/// a dark ground less (`correct_alpha`). A glyph that draws nothing has no core.
#[test]
fn a_glyphs_core_is_its_median_core_coverage() {
    let (ink, paper) = (0.0164, 1.0);
    let full = table(flat(255, 40), false);
    assert_eq!(
        full.core(GlyphInk::index('R', false), ink, paper),
        Some(1.0)
    );
    let mut thin = flat(255, 8);
    thin.levels[224] = 30;
    let raw = table(thin, false).core(GlyphInk::index('R', false), ink, paper);
    assert_eq!(raw, Some(224.0 / 255.0), "the stem's coverage: {raw:?}");
    let on_paper = table(thin, true)
        .core(GlyphInk::index('R', false), ink, paper)
        .expect("a core");
    assert!(on_paper > 0.9, "dark ink on paper, remapped: {on_paper}");
    let on_dark = table(thin, true)
        .core(GlyphInk::index('R', false), 0.6, 0.006)
        .expect("a core");
    assert!(
        on_dark < 224.0 / 255.0,
        "light ink on dark, remapped: {on_dark}"
    );
    assert_eq!(
        table(GlyphCoverage::default(), false).core(0, ink, paper),
        None
    );
}

/// A glyph's ink box is its inked pixels' box, seated where the raster's top left lies in
/// the cell; a raster with no ink has none.
#[test]
fn a_glyphs_ink_box_is_where_its_raster_seats_its_ink() {
    let raster = [0, 0, 0, 0, 9, 0, 0, 0, 7];
    let placed = GlyphCoverage::placed(&raster, 3, (2, 5));
    assert_eq!(placed.cell_box, Some([3, 6, 5, 8]));
    assert_eq!(placed.max, 9);
    assert_eq!(GlyphCoverage::placed(&[0; 9], 3, (2, 5)).cell_box, None);
    assert_eq!(GlyphCoverage::placed(&raster, 0, (2, 5)).cell_box, None);
    let mut regular = [flat(255, 40); ASCII];
    regular[usize::from(GlyphInk::index('m', false))] = placed;
    let ink = GlyphInk::from_coverage(regular, [flat(255, 40); ASCII], false);
    assert_eq!(
        ink.cell_box(GlyphInk::index('m', false)),
        Some([3, 6, 5, 8])
    );
    assert_eq!(ink.cell_box(GlyphInk::OTHER), None);
}

/// A table's fingerprint is `aterm_hash`'s word step over every glyph's fullest pixel, levels
/// and place, then every raster's words and coverage, and its value does not move: each pin
/// was read from a run of the folds as this crate spelled them out before (6ed031fa50).
#[test]
fn a_tables_fingerprint_folds_with_aterm_hashs_step() {
    let mut glyph = flat(200, 40);
    glyph.cell_box = Some([1, -2, 3, 4]);
    let coverage = table(glyph, true);
    let mut want = aterm_hash::FNV64_OFFSET ^ 1 ^ u64::from(coverage.other) << 8;
    for _ in 0..2 * ASCII {
        let place = [1i16, -2, 3, 4].map(i16::cast_unsigned);
        for v in std::iter::once(u16::from(glyph.max))
            .chain(glyph.levels)
            .chain(place)
        {
            want = aterm_hash::fnv_mix_u64(want, u64::from(v));
        }
    }
    assert_eq!(coverage.fingerprint(), want);
    assert_eq!(coverage.fingerprint(), 0x316f_a577_ad1e_1a84);

    let raster = GlyphRaster::placed(&[0, 9, 200, 0], 2, (-1, 5)).expect("ink");
    let seated = coverage.with_rasters(vec![Some(raster.clone()), None]);
    want ^= 0x9E37_79B9_7F4A_7C15;
    let words = [raster.x, raster.y, 2].map(i32::cast_unsigned);
    for v in words
        .into_iter()
        .map(u64::from)
        .chain(raster.cov.iter().map(|&c| u64::from(c)))
    {
        want = aterm_hash::fnv_mix_u64(want, v);
    }
    for v in [i32::MIN; 3].map(i32::cast_unsigned) {
        want = aterm_hash::fnv_mix_u64(want, u64::from(v));
    }
    assert_eq!(seated.fingerprint(), want);
    assert_eq!(seated.fingerprint(), 0xbea8_0a8f_ccdb_3eb6);
}

/// A glyph's raster is its inked pixels, trimmed and seated where the raster's top left lies
/// in the cell, read back pixel by pixel (none off it); the table answers each glyph's raster
/// by its index, none for a glyph it knows only as another, and the rasters reach its
/// fingerprint. A raster with no ink has none.
#[test]
fn a_glyphs_raster_is_its_inked_pixels_where_its_cell_seats_them() {
    let bytes = [0, 0, 0, 0, 0, 9, 200, 0, 0, 0, 255, 0];
    let raster = GlyphRaster::placed(&bytes, 4, (2, 5)).expect("ink");
    assert_eq!((raster.x, raster.y, raster.w, raster.h()), (3, 6, 2, 2));
    assert_eq!(&*raster.cov, &[9, 200, 0, 255]);
    assert_eq!(raster.at(3, 6), 9);
    assert_eq!(raster.at(4, 6), 200);
    assert_eq!(raster.at(4, 7), 255);
    assert_eq!(raster.at(2, 6), 0);
    assert_eq!(raster.at(3, 8), 0);
    assert_eq!(raster.at(5, 6), 0);
    assert_eq!(GlyphRaster::placed(&[0; 12], 4, (2, 5)), None);
    let plain = table(flat(255, 40), false);
    let mut rasters = vec![None; 2 * ASCII];
    rasters[usize::from(GlyphInk::index('m', false))] = Some(raster.clone());
    let seated = plain.clone().with_rasters(rasters);
    assert_eq!(seated.raster(GlyphInk::index('m', false)), Some(&raster));
    assert_eq!(seated.raster(GlyphInk::index('n', false)), None);
    assert_eq!(seated.raster(GlyphInk::OTHER), None);
    assert_eq!(plain.raster(GlyphInk::index('m', false)), None);
    assert_ne!(seated.fingerprint(), plain.fingerprint());
}

/// The thin share is the thinnest glyph's: `1 −` the least fullest-pixel coverage, remapped
/// as the renderer lays it. A face whose every glyph lays a pixel at full coverage has none,
/// and so does one that draws nothing; a blank glyph (the space) is not the thinnest.
#[test]
fn the_thin_share_is_the_thinnest_glyphs_darkest_pixel() {
    let (ink, paper) = (0.0164, 1.0);
    assert_eq!(table(flat(255, 40), false).thin_share(ink, paper), 0.0);
    assert_eq!(
        table(GlyphCoverage::default(), true).thin_share(ink, paper),
        0.0
    );
    let mut regular = [flat(255, 40); ASCII];
    regular[0] = GlyphCoverage::default();
    regular[usize::from(GlyphInk::index('"', false))] = flat(250, 6);
    regular[usize::from(GlyphInk::index('_', false))] = flat(240, 12);
    let raw = GlyphInk::from_coverage(regular, [flat(255, 40); ASCII], false);
    assert!(
        (raw.thin_share(ink, paper) - 15.0 / 255.0).abs() < 1e-6,
        "the `_`'s 240: {}",
        raw.thin_share(ink, paper)
    );
    let remapped = GlyphInk::from_coverage(regular, [flat(255, 40); ASCII], true);
    let want = 1.0 - crate::correct_alpha(ink, paper, 240.0 / 255.0);
    assert!((remapped.thin_share(ink, paper) - want).abs() < 1e-6);
    assert!(
        remapped.thin_share(ink, paper) < raw.thin_share(ink, paper),
        "dark ink on paper covers more than its raw coverage"
    );
}

/// Printable ASCII reads by its code, bold after regular; anything else as the thinnest
/// alphanumeric.
#[test]
fn a_cell_reads_its_own_glyph_or_the_thinnest_letter() {
    assert_eq!(GlyphInk::index(' ', false), 0);
    assert_eq!(GlyphInk::index('~', true), 2 * ASCII as u8 - 1);
    assert_eq!(GlyphInk::index('你', false), GlyphInk::OTHER);
    let mut regular = [flat(255, 40); ASCII];
    regular[usize::from(GlyphInk::index('l', false))] = flat(128, 20);
    let ink = GlyphInk::from_coverage(regular, [flat(255, 40); ASCII], false);
    assert_eq!(
        ink.core(GlyphInk::OTHER, 0.0164, 1.0),
        ink.core(GlyphInk::index('l', false), 0.0164, 1.0)
    );
}
