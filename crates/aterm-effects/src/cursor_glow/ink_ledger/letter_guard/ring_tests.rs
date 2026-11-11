// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::RasterHold;
use super::*;

const PAGE: u32 = 0x0000_2B36;
const INK: u32 = 0x0083_9496;
/// Two lights at the page's lightness in other hues: each channel's median of the two (the
/// upper of an even count) is brighter than either.
const WARM: u32 = 0x0050_2B36;
const TEAL: u32 = 0x0000_4A36;

fn ratio(a: u32, b: u32) -> f32 {
    let t = crate::color_math::relative_luminance_table();
    let (la, lb) = (
        crate::color_math::relative_luminance_with(t, a),
        crate::color_math::relative_luminance_with(t, b),
    );
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

struct Px<'a>(&'a [u32]);
impl FramePx for Px<'_> {
    fn at(&self, k: usize) -> u32 {
        self.0[k]
    }
}

/// The judges' reading, written out: each solid pixel (`solid`) against each channel's
/// median (the upper of an even count) of the frame on its nearest ring of `ground`, each
/// row's lower median, the worst row.
fn judged(frame: &[u32], w: usize, solid: &[usize], ground: &[bool]) -> f32 {
    let mut rows: Vec<(usize, f32)> = Vec::new();
    for &k in solid {
        let (x, y) = ((k % w) as i64, (k / w) as i64);
        let h = (frame.len() / w) as i64;
        let ring = (1..=2i64).find_map(|d| {
            let px: Vec<u32> = (-d..=d)
                .flat_map(|dy| (-d..=d).map(move |dx| (dx, dy)))
                .filter(|&(dx, dy)| dx.abs().max(dy.abs()) == d)
                .map(|(dx, dy)| (x + dx, y + dy))
                .filter(|&(xx, yy)| xx >= 0 && yy >= 0 && xx < w as i64 && yy < h)
                .map(|(xx, yy)| yy as usize * w + xx as usize)
                .filter(|&kk| ground[kk])
                .map(|kk| frame[kk])
                .collect();
            (!px.is_empty()).then_some(px)
        });
        let Some(ring) = ring else { continue };
        let median = [16u32, 8, 0].iter().fold(0, |out, &sh| {
            let mut c: Vec<u32> = ring.iter().map(|p| (p >> sh) & 0xff).collect();
            c.sort_unstable();
            out | c[c.len() / 2] << sh
        });
        rows.push((y as usize, ratio(frame[k], median)));
    }
    let mut worst = f32::INFINITY;
    for line in rows.chunk_by(|a, b| a.0 == b.0) {
        let mut r: Vec<f32> = line.iter().map(|p| p.1).collect();
        r.sort_by(f32::total_cmp);
        worst = worst.min(r[(r.len() - 1) / 2]);
    }
    worst
}

/// Every part of `kept`'s read and `fresh`'s, a reading of each on a frame, and where
/// [`RingCell::holds`] stands either side of it.
fn same_read(kept: &mut RingCell, fresh: &mut RingCell, frame: &[u32], what: &str) {
    let (k, f) = (kept.current(), fresh.current());
    assert_eq!(k.text, f.text, "{what}: its text");
    assert_eq!(k.text_px, f.text_px, "{what}: its text pixels");
    assert_eq!(k.plain_px, f.plain_px, "{what}: its plain pixels");
    assert_eq!(k.solid, f.solid, "{what}: its solid ink");
    assert_eq!(k.ring_at, f.ring_at, "{what}: its rings' starts");
    assert_eq!(k.rings, f.rings, "{what}: its rings");
    assert_eq!(k.filled, f.filled, "{what}: whether it is filled");
    assert_eq!(k.core, f.core, "{what}: its core");
    assert_eq!(k.bed, f.bed, "{what}: its bed");
    for (a, b, part) in [
        (k.own, f.own, "own reading"),
        (k.l_min, f.l_min, "least ink"),
        (k.whole_own, f.whole_own, "whole reading's own"),
        (k.l_core, f.l_core, "core's ink"),
        (kept.own, f.own, "own reading kept"),
        (kept.l_min, f.l_min, "least ink kept"),
    ] {
        assert_eq!(a.to_bits(), b.to_bits(), "{what}: its {part}: {a} {b}");
    }
    assert_eq!(k.ring_text, f.ring_text, "{what}: its ring text");
    assert_eq!(kept.ring_text, f.ring_text, "{what}: its ring text kept");
    let read = kept.reading(&Px(frame));
    assert_eq!(
        read.to_bits(),
        fresh.reading(&Px(frame)).to_bits(),
        "{what}: its reading"
    );
    for (bar, holds) in [
        (read.next_down(), true),
        (read, true),
        (read.next_up(), read.is_infinite()),
    ] {
        assert_eq!(kept.holds(&Px(frame), bar), holds, "{what}: holds {bar}");
    }
    assert_eq!(
        kept.whole(&Px(frame)).to_bits(),
        fresh.whole(&Px(frame)).to_bits(),
        "{what}: its whole reading"
    );
    assert_eq!(
        kept.whole_holds(&Px(frame), RasterHold::Bar),
        fresh.whole_holds(&Px(frame), RasterHold::Bar),
        "{what}: whether its whole reading holds"
    );
}

/// A NEIGHBOURHOOD READ BEFORE IS TAKEN AS IT WAS READ, AND ONLY THEN. Over neighbourhoods
/// differing in one part each (a cell's ground, base, glyph, colour, or being opaque, a box or
/// cell or ring size, the blend), read twice, more of them than are kept, and on another
/// table at the same glyph indices, a cell that keeps its reads holds exactly what a new one
/// reads, and its reads stay within [`READ_SLOTS`]; the neighbourhood just read, and most of a
/// second pass, are not read again. NEGATIVE CONTROLS (2026-11-05), each fails here on that
/// part of the key's neighbourhood: a key blind to `base` on its text pixels, to the blend or a
/// glyph's colour on its text, and a table change that keeps the reads on another table's text.
#[test]
fn a_neighbourhood_read_before_is_taken_as_it_was_read() {
    let (cw, ch, r) = (6usize, 10usize, 2usize);
    let raster = |cols: [usize; 2], rows: std::ops::Range<usize>, cov: u8| {
        let mut c = vec![0u8; cw * ch];
        for y in rows {
            for x in cols {
                c[y * cw + x] = cov;
            }
        }
        c[(ch - 1) * cw + cols[0]] = 90;
        Some(GlyphRaster::placed(&c, cw, (0, 0)).expect("ink"))
    };
    let table: RasterTable = vec![
        raster([2, 2], 2..8, 255),
        raster([0, 5], 1..9, 200),
        raster([1, 3], 3..6, 255),
    ]
    .into();
    let other: RasterTable = vec![
        raster([3, 4], 1..7, 255),
        raster([1, 1], 0..4, 160),
        raster([0, 2], 5..9, 255),
    ]
    .into();
    let known = |ground, glyph| Neighbour::Known {
        ground,
        base: ground,
        glyph,
    };
    let mut fixture = [known(PAGE, None); 9];
    fixture[4] = known(PAGE, Some((0, INK)));
    fixture[5] = known(PAGE, Some((1, INK)));
    fixture[7] = known(WARM, None);
    let sizes = [
        (cw + 2 * r, ch + 2 * r, cw, ch, r),
        (cw + 2, ch + 2, cw, ch, 1),
        (cw + 2 * r, ch + 2 * r + 2, cw, ch + 2, r),
    ];
    type Hood = ((usize, usize, usize, usize, usize), [Neighbour; 9], bool);
    let mut hoods: Vec<Hood> = vec![(sizes[0], fixture, false)];
    hoods.extend(sizes[1..].iter().map(|&s| (s, fixture, false)));
    hoods.push((sizes[0], fixture, true));
    for at in 0..9 {
        let Neighbour::Known {
            ground,
            base,
            glyph,
        } = fixture[at]
        else {
            continue;
        };
        for changed in [
            Neighbour::Known {
                ground: TEAL,
                base,
                glyph,
            },
            Neighbour::Known {
                ground,
                base: TEAL,
                glyph,
            },
            Neighbour::Known {
                ground,
                base,
                glyph: Some((2, INK)),
            },
            Neighbour::Known {
                ground,
                base,
                glyph: Some((0, WARM)),
            },
            Neighbour::Opaque,
        ] {
            let mut cells = fixture;
            cells[at] = changed;
            hoods.push((sizes[0], cells, false));
        }
    }
    // A frame of some other colours, for the readings.
    let mut x = 0x9E37_79B9u32;
    let mut frame = vec![0u32; 256 * 16];
    for px in &mut frame {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        *px = [PAGE, WARM, TEAL, INK][(x % 4) as usize];
    }
    let mut kept = RingCell::EMPTY;
    let read = |cell: &mut RingCell, hood: &Hood, table: &RasterTable| {
        let &((w, h, cw, ch, r), cells, corrected) = hood;
        cell.read((w, h), (cw, ch), r, &cells, (table, corrected));
    };
    let mut first_pass = 0;
    for pass in 0..2 {
        if pass == 1 {
            first_pass = kept.anew;
        }
        for (i, hood) in hoods.iter().enumerate() {
            read(&mut kept, hood, &table);
            let mut fresh = RingCell::EMPTY;
            read(&mut fresh, hood, &table);
            same_read(
                &mut kept,
                &mut fresh,
                &frame,
                &format!("pass {pass} hood {i}"),
            );
            let anew = kept.anew;
            read(&mut kept, hood, &table);
            assert_eq!(
                kept.anew, anew,
                "hood {i}: read again as soon as it was read"
            );
        }
    }
    let again = kept.anew - first_pass;
    assert!(
        again < hoods.len() / 2,
        "a second pass read {again} of {} neighbourhoods again",
        hoods.len()
    );
    // Another table: the same key, other rasters.
    let hood = &hoods[0];
    read(&mut kept, hood, &table);
    let of_table = kept.current().text.clone();
    read(&mut kept, hood, &other);
    let mut fresh = RingCell::EMPTY;
    read(&mut fresh, hood, &other);
    same_read(&mut kept, &mut fresh, &frame, "another table");
    assert_ne!(kept.current().text, of_table, "fixture: the tables differ");
    // More neighbourhoods than are kept.
    for i in 0..3 * READ_SLOTS as u32 {
        let mut cells = fixture;
        cells[0] = known(0x0010_0000 + i, None);
        let hood = (sizes[0], cells, false);
        read(&mut kept, &hood, &other);
        let mut fresh = RingCell::EMPTY;
        read(&mut fresh, &hood, &other);
        same_read(&mut kept, &mut fresh, &frame, &format!("sweep {i}"));
        assert_eq!(kept.reads.len(), READ_SLOTS, "its reads stay bounded");
    }
}

/// [`RingCell::holds`] STOPS ONLY AT A ROW UNDER THE BAR: over a stem whose rows read lower
/// and lower, at each row's reading and either side of it, it answers as the whole reading
/// does. NEGATIVE CONTROL (2026-11-05): a stop at `worst <= stop` takes a first row at the bar
/// for the reading and fails here.
#[test]
fn holds_answers_as_the_whole_reading_at_every_row_s_reading() {
    let (cw, ch, r) = (6usize, 10usize, 2usize);
    let (w, h) = (cw + 2 * r, ch + 2 * r);
    let mut stem = vec![0u8; cw * ch];
    for y in 2..8 {
        stem[y * cw + 2] = 255;
    }
    let table: RasterTable = vec![GlyphRaster::placed(&stem, cw, (0, 0))].into();
    let mut cells = [Neighbour::Known {
        ground: PAGE,
        base: PAGE,
        glyph: None,
    }; 9];
    cells[4] = Neighbour::Known {
        ground: PAGE,
        base: PAGE,
        glyph: Some((0, 0x00FF_FFFF)),
    };
    let mut cell = RingCell::EMPTY;
    cell.read((w, h), (cw, ch), r, &cells, (&table, false));
    let mut frame = vec![PAGE; w * h];
    let inks: Vec<u32> = (0..6u32).map(|i| 0x00F0_F0F0 - i * 0x0020_2020).collect();
    for (i, ink) in inks.iter().enumerate() {
        frame[(r + 2 + i) * w + r + 2] = *ink;
    }
    let reading = cell.reading(&Px(&frame));
    assert_eq!(
        reading.to_bits(),
        ratio(inks[5], PAGE).to_bits(),
        "the last row is the worst"
    );
    for bar in inks.iter().map(|ink| ratio(*ink, PAGE)) {
        for bar in [bar.next_down(), bar, bar.next_up()] {
            assert_eq!(
                cell.holds(&Px(&frame), bar),
                reading >= bar,
                "a bar of {bar} on a reading of {reading}"
            );
        }
    }
}

/// EACH CHANNEL'S MEDIAN IS THE ONE A SORTED LIST READS: the upper one of an even count, each
/// channel its own, on both sides of the length where the reader stops ordering and counts,
/// with values from all alike to all different.
#[test]
fn each_channel_s_median_is_the_one_a_sorted_list_reads() {
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed as u32
    };
    let lens = (1..=RING_MOST + 2).chain([255, 256, 257, 720, 2880]);
    for len in lens {
        for spread in [1u32, 2, 7, 256] {
            for _ in 0..8 {
                let frame: Vec<u32> = (0..len)
                    .map(|_| {
                        let (a, b, c) = (next() % spread, next() % spread, next() % spread);
                        (next() & 0xFF00_0000) | a << 16 | b << 8 | c
                    })
                    .collect();
                let at: Vec<u32> = (0..len as u32).rev().collect();
                let mut want = 0;
                for sh in [16u32, 8, 0] {
                    let mut channel: Vec<u32> = frame.iter().map(|v| (v >> sh) & 0xFF).collect();
                    channel.sort_unstable();
                    want |= channel[len / 2] << sh;
                }
                assert_eq!(
                    median_of(&at, &Px(&frame)),
                    want,
                    "{len} pixels of {spread} values"
                );
            }
        }
    }
    assert_eq!(median_of(&[], &Px(&[])), 0, "over none");
}

/// A TINY GLYPH'S WHOLE READING IS ITS CORE AGAINST ITS BED, AS THE JUDGES READ IT: a `.` on
/// paper. Its solid ink keeps its ring on a frame that lights the cell's upper rows, so the
/// ring's reading stays its own; the whole reading (the core's channel medians against the
/// bed's) falls to what `trail-contrast`'s `contrast` computes, under its bar, and the frame
/// fails [`RingCell::whole_holds`]. Its bar is its own share, the dot's core reading under
/// the floor though its solid ink reads far over it.
#[test]
fn a_tiny_glyph_reads_its_core_against_its_bed() {
    const PAPER: u32 = 0x00FF_FFFF;
    const DOT: u32 = 0x001F_2328;
    const FLAME: u32 = 0x00FF_C080;
    let (cw, ch, r) = (6usize, 10usize, 2usize);
    let (w, h) = (cw + 2 * r, ch + 2 * r);
    let mut dot = vec![0u8; cw * ch];
    // The real `.` of the Cursor card's `README.md`, its coverage read off the clean frame.
    dot[7 * cw + 2..7 * cw + 4].copy_from_slice(&[195, 30]);
    dot[8 * cw + 1..8 * cw + 4].copy_from_slice(&[243, 255, 188]);
    dot[9 * cw + 1..9 * cw + 4].copy_from_slice(&[205, 252, 100]);
    let table: RasterTable = vec![Some(GlyphRaster::placed(&dot, cw, (0, 0)).expect("ink"))].into();
    let blank = Neighbour::Known {
        ground: PAPER,
        base: PAPER,
        glyph: None,
    };
    let mut cells = [blank; 9];
    cells[4] = Neighbour::Known {
        ground: PAPER,
        base: PAPER,
        glyph: Some((0, DOT)),
    };
    let mut cell = RingCell::EMPTY;
    cell.read((w, h), (cw, ch), r, &cells, (&table, false));
    let clean = cell.clean.clone();
    assert_eq!(
        cell.whole_bar(RasterHold::Bar),
        super::super::raster_bar(cell.whole(&Px(&clean)).min(cell.own)),
        "the bar is what the whole reading reads on the clean frame"
    );
    let own = cell.reading(&Px(&clean));
    assert!(
        own > 4.6 && cell.whole_bar(RasterHold::Bar) < 4.6,
        "fixture: the ring reads {own}, the core's bar is {}",
        cell.whole_bar(RasterHold::Bar)
    );
    assert!(
        cell.whole_holds(&Px(&clean), RasterHold::Bar),
        "the clean frame keeps its bar"
    );
    // The upper half of the cell burns; the row over the dot and everything lower does not.
    let mut frame = clean.clone();
    for (k, px) in frame.iter_mut().enumerate() {
        if cell.ground[k] && k / w < r + 6 {
            *px = FLAME;
        }
    }
    assert_eq!(cell.reading(&Px(&frame)), own, "its ring is untouched");
    let core: Vec<u32> = cell
        .current()
        .core
        .iter()
        .map(|&k| frame[k as usize])
        .collect();
    let bed: Vec<u32> = cell
        .current()
        .bed
        .iter()
        .map(|&k| frame[k as usize])
        .collect();
    let med = |px: &[u32]| {
        [16u32, 8, 0].iter().fold(0, |out, &sh| {
            let mut c: Vec<u32> = px.iter().map(|p| (p >> sh) & 0xff).collect();
            c.sort_unstable();
            out | c[c.len() / 2] << sh
        })
    };
    let want = ratio(med(&core), med(&bed));
    let read = cell.whole(&Px(&frame));
    assert!(
        (read - want).abs() < 1e-5,
        "the cell's whole reading {read}, the judges' {want}"
    );
    assert!(
        !cell.whole_holds(&Px(&frame), RasterHold::Bar),
        "a flame over the cell takes its bed: the whole reading {read} against its bar {}",
        cell.whole_bar(RasterHold::Bar)
    );
}

/// A STROKE READS AGAINST ITS RING'S CHANNEL MEDIANS, AS THE JUDGES READ IT: a cell read from
/// its glyph's own raster finds the judges' solid ink and rings (the nearest ring of its
/// ground, a neighbour's glyph and a block caret's cell no ground), reads its own reading on
/// the clean frame (the ink against the page), and on a frame whose ground takes a warm light
/// left of the stroke and a teal one right of it, at the page's lightness, it reads exactly
/// what the judges read: under the reading against either light alone, each channel's median
/// of the two being brighter than both (round 25: Nyan's `p` at 96.6% of its own reading on
/// Solarized Dark, which no bound on the grounds' luminance saw).
#[test]
fn a_stroke_reads_against_its_ring_s_channel_medians() {
    let (cw, ch, r) = (6usize, 10usize, 2usize);
    let (w, h) = (cw + 2 * r, ch + 2 * r);
    // A one-pixel stem, full coverage, rows 2..8 of its cell.
    let mut stem = vec![0u8; cw * ch];
    for y in 2..8 {
        stem[y * cw + 2] = 255;
    }
    let stem = GlyphRaster::placed(&stem, cw, (0, 0)).expect("ink");
    // The neighbour to the right lays a pixel of ink at its left edge, beside the stem's
    // ring; the neighbour below is a block caret's fill.
    let mut dot = vec![0u8; cw * ch];
    dot[4 * cw] = 255;
    let dot = GlyphRaster::placed(&dot, cw, (0, 0)).expect("ink");
    let table: RasterTable = vec![Some(stem), Some(dot)].into();
    let blank = Neighbour::Known {
        ground: PAGE,
        base: PAGE,
        glyph: None,
    };
    let mut cells = [blank; 9];
    cells[4] = Neighbour::Known {
        ground: PAGE,
        base: PAGE,
        glyph: Some((0, INK)),
    };
    cells[5] = Neighbour::Known {
        ground: PAGE,
        base: PAGE,
        glyph: Some((1, INK)),
    };
    cells[7] = Neighbour::Opaque;
    let mut cell = RingCell::EMPTY;
    cell.read((w, h), (cw, ch), r, &cells, (&table, false));
    let own = ratio(INK, PAGE);
    assert!(
        (cell.own - own).abs() < 1e-4,
        "its own reading {} is the ink against the page {own}",
        cell.own
    );
    let solid: Vec<usize> = (0..w * h)
        .filter(|&k| {
            let (x, y) = (k % w, k / w);
            x == r + 2 && (r + 2..r + 8).contains(&y)
        })
        .collect();
    assert_eq!(
        cell.current()
            .solid
            .iter()
            .map(|&(k, _)| k as usize)
            .collect::<Vec<_>>(),
        solid,
        "the solid ink is the stem's"
    );
    let ground: Vec<bool> = (0..w * h).map(|k| cell.ground[k]).collect();
    let dot_px = (r + 4) * w + r + cw;
    assert!(!ground[dot_px], "a neighbour's ink is no ground");
    assert!(
        !ground[(r + ch) * w + r + 2],
        "a block caret's cell is no ground"
    );
    // The ground lit warm up to the stem's column and teal past it.
    let mut frame = cell.clean.clone();
    for (k, px) in frame.iter_mut().enumerate() {
        if ground[k] {
            *px = if k % w <= r + 2 { WARM } else { TEAL };
        }
    }
    let read = cell.reading(&Px(&frame));
    let want = judged(&frame, w, &solid, &ground);
    assert!(
        (read - want).abs() < 1e-5,
        "the cell reads {read}, the judges {want}"
    );
    let alone = ratio(INK, WARM).min(ratio(INK, TEAL));
    assert!(
        read < alone,
        "fixture: the ring's medians read brighter than either light ({read} against {alone})"
    );
}
