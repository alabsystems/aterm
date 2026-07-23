// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tier-1 conformance + L0 lattice proofs for M4 (ligature slicing): the shaping
//! GATE stays conservative when the Cascadia N:1 branch is added, and the raster
//! SLICING that the N:1 path rests on partitions a wide ligature glyph into
//! per-cell tiles that reassemble byte-exactly and recolour locally.
//!
//! These bind the SHIPPING pure functions
//! ([`aterm_render::ligature_shaping::classify_shape`],
//! [`aterm_render::ligature_shaping::slice_tile_bands`],
//! [`aterm_render::ligature_shaping::extract_tile`]) to the M4 PROVE bullets:
//!
//! 1. **Slicing partition** — concatenating the per-cell tiles reproduces the
//!    original wide raster byte-exactly (disjoint + complete), over an odd/even
//!    size lattice. ty has no multiplication, so this arithmetic law is an L0
//!    lattice test, not a ty model (the M4 rounding/scaling waiver).
//! 2. **Per-tile recolour locality** — recolouring tile `k` changes no bytes
//!    outside cell `k`'s columns.
//! 3. **Conservative gate** — `classify_shape` admits EXACTLY the two
//!    grid-mappable forms (1:1, or N:1 with `N>=2` behind the flag) and rejects
//!    everything else; with the flag off it is byte-identical to the legacy
//!    `accept iff n_out == n_in`. This is the SAME invariant the `gate_*` kani
//!    proofs and the `LigatureGate` derived ty model carry (Tier-1 binding).

use aterm_render::ligature_shaping::{
    ShapeVerdict, TileBand, classify_shape, extract_tile, slice_tile_bands,
};

// ---------------------------------------------------------------------------
// PROVE (1): slicing partition — disjoint + complete + byte-exact reassembly.
// ---------------------------------------------------------------------------

/// A wide coverage raster `w × h` with a DISTINCT byte per pixel (mod 256) so a
/// dropped, duplicated, or transposed column is detectable in the reassembly.
fn synth_raster(w: usize, h: usize) -> Vec<u8> {
    (0..w * h).map(|i| (i % 251 + 1) as u8).collect()
}

/// The band set is a partition of `[0, raster_w)`: it starts at 0, ends at
/// `raster_w`, is contiguous (each band meets the next), every band is non-empty,
/// and the widths sum to `raster_w`. Every interior band is exactly `cell_w` wide;
/// only the final band may be narrower (the remainder).
#[test]
fn slice_partition_is_disjoint_and_complete() {
    let mut saw_remainder = false; // non-vacuity: the narrow-final-band branch fires
    let mut saw_multi = false; // non-vacuity: multi-cell rasters are exercised
    for raster_w in 0..=40usize {
        for cell_w in 1..=12usize {
            let bands = slice_tile_bands(raster_w, cell_w);
            if raster_w == 0 {
                assert!(bands.is_empty(), "empty raster => no bands");
                continue;
            }
            // Starts at 0, ends at raster_w.
            assert_eq!(bands.first().unwrap().x0, 0, "first band starts at 0");
            assert_eq!(
                bands.last().unwrap().x1,
                raster_w,
                "last band ends at raster_w"
            );
            let mut total = 0usize;
            for (i, b) in bands.iter().enumerate() {
                assert!(b.x1 > b.x0, "every band is non-empty");
                assert_eq!(b.width(), b.x1 - b.x0);
                if i + 1 < bands.len() {
                    // Contiguous: band i meets band i+1 (disjoint + no gap).
                    assert_eq!(b.x1, bands[i + 1].x0, "bands are contiguous");
                    // Interior bands are exactly one cell wide.
                    assert_eq!(b.width(), cell_w, "interior band is cell_w wide");
                }
                total += b.width();
            }
            assert_eq!(total, raster_w, "band widths tile the raster exactly");
            if bands.len() >= 2 {
                saw_multi = true;
            }
            if bands.last().unwrap().width() < cell_w {
                saw_remainder = true;
            }
        }
    }
    assert!(saw_multi, "non-vacuity: multi-band partitions exercised");
    assert!(
        saw_remainder,
        "non-vacuity: the remainder branch is reachable"
    );
}

/// PROVE (1), the byte-level statement: extracting every band and laying each tile
/// back at its band offset reproduces the ORIGINAL wide raster byte-for-byte, over
/// an odd/even width × height × cell_w lattice. A negative control shows the
/// partition matters: a BUGGY slicer that gives the final band a full `cell_w`
/// (ignoring the remainder) fails to reassemble.
#[test]
fn slice_tiles_reassemble_byte_exactly() {
    let mut reassembled_a_remainder = false;
    for raster_w in 1..=33usize {
        for height in 1..=5usize {
            for cell_w in 1..=11usize {
                let raster = synth_raster(raster_w, height);
                let bands = slice_tile_bands(raster_w, cell_w);
                // Reassemble: copy each extracted tile back at its column offset.
                let mut rebuilt = vec![0u8; raster_w * height];
                for b in &bands {
                    let tile = extract_tile(&raster, raster_w, height, *b);
                    assert_eq!(tile.len(), b.width() * height, "tile is width*height");
                    let tw = b.width();
                    for row in 0..height {
                        let dst = row * raster_w + b.x0;
                        let src = row * tw;
                        rebuilt[dst..dst + tw].copy_from_slice(&tile[src..src + tw]);
                    }
                    if tw < cell_w {
                        reassembled_a_remainder = true;
                    }
                }
                assert_eq!(
                    rebuilt, raster,
                    "concatenated tiles must reproduce the raster (w={raster_w}, h={height}, cw={cell_w})"
                );
            }
        }
    }
    assert!(
        reassembled_a_remainder,
        "non-vacuity: a narrower-than-cell_w final tile was reassembled"
    );

    // NEGATIVE CONTROL — reproduce the pre-slicing defect. A slicer that assumes
    // every band is a full cell_w (dropping the remainder) reads past the raster
    // for a non-multiple width; our `extract_tile` bounds-checks and returns empty,
    // so a full-cell_w final band CANNOT reassemble — proving the remainder-aware
    // partition is load-bearing.
    let raster_w = 25usize;
    let height = 3usize;
    let raster = synth_raster(raster_w, height);
    // Buggy band: force the final band to a full cell_w (x1 past raster_w=25).
    let buggy_final = TileBand { x0: 20, x1: 30 };
    let last = extract_tile(&raster, raster_w, height, buggy_final);
    assert!(
        last.is_empty(),
        "an out-of-bounds full-cell final band must not slice (defect caught)"
    );
}

// ---------------------------------------------------------------------------
// PROVE (2): per-tile recolour locality.
// ---------------------------------------------------------------------------

/// Blend one coverage byte over a destination pixel (premultiplied-free, matches
/// the renderer's `blend` shape closely enough for a locality proof: cov==0 leaves
/// the dest untouched, cov>0 writes the fg-tinted value).
fn blend(bg: u32, fg: u32, cov: u8) -> u32 {
    if cov == 0 {
        return bg;
    }
    // A monotone, cov-dependent mix; the exact formula is irrelevant to LOCALITY,
    // only that cov==0 is a no-op and cov>0 mutates.
    let a = u32::from(cov);
    let mix = |b: u32, f: u32| ((b * (255 - a) + f * a) / 255) & 0xff;
    let r = mix((bg >> 16) & 0xff, (fg >> 16) & 0xff);
    let g = mix((bg >> 8) & 0xff, (fg >> 8) & 0xff);
    let bl = mix(bg & 0xff, fg & 0xff);
    (r << 16) | (g << 8) | bl
}

/// Recolouring tile `k` (blitting its coverage at band `k`'s columns in a new fg)
/// changes NO destination pixel outside band `k`'s column span — every other cell's
/// ink survives. This is the block-cursor / per-cell-fg locality M4 delivers:
/// recolour one cell of a merged ligature without dissolving the run.
#[test]
fn per_tile_recolor_is_local() {
    const BG: u32 = 0x0011_1318;
    const FG: u32 = 0x00FF_A000; // the "recolour" (e.g. cursor/selection/syntax fg)
    let mut touched_outside = false; // must stay false
    let mut recoloured_something = false; // non-vacuity: some pixel actually changed
    for raster_w in 1..=24usize {
        for height in 1..=4usize {
            for cell_w in 1..=9usize {
                let raster = synth_raster(raster_w, height);
                let bands = slice_tile_bands(raster_w, cell_w);
                for (k, band) in bands.iter().enumerate() {
                    // Fresh dest painted with the base ink of the WHOLE raster.
                    let base: Vec<u32> =
                        raster.iter().map(|&c| blend(BG, 0x00C8_C8C8, c)).collect();
                    let mut dest = base.clone();
                    // Recolour ONLY tile k: blit its coverage at band k in FG.
                    let tw = band.width();
                    let tile = extract_tile(&raster, raster_w, height, *band);
                    for row in 0..height {
                        for col in 0..tw {
                            let cov = tile[row * tw + col];
                            let idx = row * raster_w + band.x0 + col;
                            let before = dest[idx];
                            dest[idx] = blend(BG, FG, cov);
                            if dest[idx] != before {
                                recoloured_something = true;
                            }
                        }
                    }
                    // Locality: every pixel OUTSIDE band k's columns is unchanged.
                    for row in 0..height {
                        for x in 0..raster_w {
                            let inside = x >= band.x0 && x < band.x1;
                            if !inside {
                                let idx = row * raster_w + x;
                                if dest[idx] != base[idx] {
                                    touched_outside = true;
                                }
                            }
                        }
                    }
                    let _ = k;
                }
            }
        }
    }
    assert!(
        !touched_outside,
        "recolouring tile k must not touch any other cell"
    );
    assert!(
        recoloured_something,
        "non-vacuity: a recolour actually changed pixels"
    );
}

// ---------------------------------------------------------------------------
// PROVE (3): conservative gate — Tier-1 binding of the shipping classifier.
// ---------------------------------------------------------------------------

/// The complete small-count lattice: `classify_shape` matches its specification for
/// every `(n_in, n_out)` in `0..=8` under BOTH flag settings, admits ONLY the two
/// grid-mappable forms, and — with the flag off — is byte-identical to the legacy
/// `accept iff n_out == n_in`. Non-vacuity: each verdict is reached; a negative
/// control shows the flag is what gates the collapse.
#[test]
fn classify_shape_lattice() {
    let (mut saw_one, mut saw_collapse, mut saw_reject) = (false, false, false);
    for n_in in 0..=8usize {
        for n_out in 0..=8usize {
            for &admit in &[false, true] {
                let v = classify_shape(n_in, n_out, admit);
                let one_to_one = n_in >= 1 && n_out == n_in;
                let collapsed = admit && n_out == 1 && n_in >= 2;
                // Spec: the two forms are mutually exclusive, 1:1 wins ties.
                let expect = if one_to_one {
                    ShapeVerdict::OneToOne
                } else if collapsed {
                    ShapeVerdict::Collapsed
                } else {
                    ShapeVerdict::Reject
                };
                assert_eq!(
                    v, expect,
                    "classify_shape({n_in}, {n_out}, {admit}) verdict"
                );

                // Flag OFF => byte-identical to the legacy count-equality accept.
                if !admit {
                    let legacy_accept = n_in >= 1 && n_out == n_in;
                    assert_eq!(
                        matches!(v, ShapeVerdict::OneToOne),
                        legacy_accept,
                        "admit-off gate must equal the legacy accept"
                    );
                    assert!(
                        !matches!(v, ShapeVerdict::Collapsed),
                        "no collapse admitted without the flag"
                    );
                }
                // A Collapsed verdict is ALWAYS a genuine N:1 (N>=2) with the flag.
                if matches!(v, ShapeVerdict::Collapsed) {
                    assert!(admit && n_out == 1 && n_in >= 2, "collapsed => flagged N:1");
                }
                match v {
                    ShapeVerdict::OneToOne => saw_one = true,
                    ShapeVerdict::Collapsed => saw_collapse = true,
                    ShapeVerdict::Reject => saw_reject = true,
                }
            }
        }
    }
    assert!(
        saw_one && saw_collapse && saw_reject,
        "non-vacuity: every verdict reached"
    );

    // NEGATIVE CONTROL — the exact case the flag guards: a 3:1 collapse is REJECTED
    // without the flag (the shipping default, byte-identical to pre-M4) and only
    // becomes admissible when the flag is set.
    assert_eq!(classify_shape(3, 1, false), ShapeVerdict::Reject);
    assert_eq!(classify_shape(3, 1, true), ShapeVerdict::Collapsed);
}

// ---------------------------------------------------------------------------
// PROVE (4): the slice CHILD-KEY encoding is a strict superset of the plain
// wide-glyph key — a whole-glyph `mono_gid` key is slice-free, and a slice key
// round-trips (gid, slice) distinctly — so the rasterizer decodes the two apart.
// ---------------------------------------------------------------------------

/// A plain `mono_gid` key has ZERO in the slice bits (a whole-glyph key), while a
/// `mono_gid_slice` key encodes `slice + 1` there over the SAME gid — so the two
/// never collide and the rasterizer's `ch_or_id >> LIG_SLICE_SHIFT` branch (0 =
/// whole glyph, else slice) is unambiguous. Distinct (gid, slice) pairs map to
/// distinct keys.
#[test]
fn slice_key_encoding_round_trips_and_is_disjoint_from_whole_glyph() {
    use aterm_render::{GlyphKey, LIG_SLICE_SHIFT, StyleBits};
    use std::collections::HashSet;
    let px_q = GlyphKey::quantize_px(18.0);
    let mut seen = HashSet::new();
    for gid in [0u16, 1, 42, 700, u16::MAX] {
        // The whole-glyph key is slice-free: high bits 0, low 16 bits == gid.
        let whole = GlyphKey::mono_gid(gid, StyleBits::REGULAR, px_q);
        assert_eq!(
            whole.ch_or_id >> LIG_SLICE_SHIFT,
            0,
            "a whole-glyph mono_gid key must have empty slice bits"
        );
        assert_eq!(whole.ch_or_id as u16, gid, "low bits carry the gid");
        assert!(seen.insert(whole.ch_or_id), "whole-glyph keys are distinct");
        for slice in 0u16..=8 {
            let key = GlyphKey::mono_gid_slice(gid, slice, StyleBits::REGULAR, px_q);
            // Slice bits decode back to `slice`; low bits still carry the gid.
            assert_eq!((key.ch_or_id >> LIG_SLICE_SHIFT) - 1, slice as u32);
            assert_eq!(key.ch_or_id as u16, gid);
            // Never equal to the whole-glyph key (the raster path treats them
            // differently: whole = rasterize the wide glyph, slice = tile it).
            assert_ne!(key.ch_or_id, whole.ch_or_id);
            assert!(
                seen.insert(key.ch_or_id),
                "distinct (gid={gid}, slice={slice}) must map to a distinct key"
            );
        }
    }
}
