// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A round face in a 12×10 art with a two-texel transparent margin on
/// every side: row `r` of 2..8 spans `2 + |r - 5| / 2 ..= 9 - |r - 5| / 2`,
/// its columns from `keep` only.
fn face_art(keep: std::ops::Range<u32>) -> Arc<FxArt> {
    let (w, h) = (12u32, 10u32);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for r in 2..8i32 {
        let inset = (r - 5).abs() / 2;
        for c in (2 + inset)..=(9 - inset) {
            if keep.contains(&(c as u32)) {
                rgba[((r as u32 * w + c as u32) * 4 + 3) as usize] = 255;
            }
        }
    }
    Arc::new(FxArt::new(12, 10, FxTexels::Rgba8Straight, rgba).expect("a 12x10 art"))
}

/// The face drawn 1:1 into `dest`.
fn face(dest: PxBox, keep: std::ops::Range<u32>, flip_x: bool) -> IconSrc {
    IconSrc {
        art: face_art(keep),
        dest,
        flip_x,
    }
}

/// How many of `rects` cover each pixel.
fn lit(rects: &[PxBox]) -> std::collections::HashMap<(i32, i32), u32> {
    let mut m = std::collections::HashMap::new();
    for &(x0, x1, y0, y1) in rects {
        for y in y0..y1 {
            for x in x0..x1 {
                *m.entry((x, y)).or_insert(0) += 1;
            }
        }
    }
    m
}

/// The shade takes exactly the face's drawn rows out of a light and
/// leaves every other pixel lit once, the box's transparent margin with
/// it, so no notch is cut beside the face. Negative control: with no
/// art the icon is its box, and the margin goes dark.
#[test]
fn the_shade_takes_the_face_s_drawn_rows_and_keeps_its_margin() {
    let icon = (100, 112, 40, 50);
    let src = face(icon, 0..12, false);
    let mut bands = Vec::new();
    drawn_rows(icon, Some(&src), (0, 0), &mut bands);
    let face = lit(&bands);
    assert_eq!(
        face.len(),
        (2..8i32).map(|r| 8 - 2 * ((r - 5).abs() / 2)).sum::<i32>() as usize,
        "the drawn rows are the face's texels: {bands:?}"
    );
    let light = (90, 130, 30, 60);
    let mut pieces = Vec::new();
    assert!(cut_rect(light, &bands, &mut pieces));
    let after = lit(&pieces);
    for px in lit(&[light]).into_keys() {
        let want = u32::from(!face.contains_key(&px));
        assert_eq!(
            after.get(&px).copied().unwrap_or(0),
            want,
            "pixel {px:?}: lit {want} time(s) after the shade"
        );
    }
    let margin = (100, 40);
    assert!(
        after.contains_key(&margin),
        "the box's margin keeps its light"
    );
    drawn_rows(icon, None, (0, 0), &mut bands);
    assert_eq!(bands, vec![icon], "control: no art, the icon is its box");
    assert!(cut_rect(light, &bands, &mut pieces));
    assert!(
        !lit(&pieces).contains_key(&margin),
        "control: cut as its box, the margin goes dark"
    );
    assert!(
        !cut_rect((0, 50, 0, 20), &bands, &mut pieces),
        "a light clear of the icon is untouched"
    );
}

/// A stream's ramps shade as the quads they stand for: the rows the
/// face's drawn rows reach are cut as their quads are (a run whole where
/// any of its rows is reached), the rows above and below stay ramps, a
/// ramp clear of the face stays whole, all in the stream's order. A
/// stream the face reaches nothing of stays as laid.
#[test]
fn a_stream_s_ramps_shade_as_their_quads() {
    let icon = (100, 112, 40, 50);
    let mut bands = Vec::new();
    drawn_rows(icon, Some(&face(icon, 0..12, false)), (0, 0), &mut bands);
    let ends = |k: u16, a: u8| {
        let c = 0x0020_3040 + u32::from(k) * 0x0003_0201;
        (
            aterm_render::glow_end(c, a),
            aterm_render::glow_end(c ^ 0x0011_1111, a / 2),
        )
    };
    let quads = vec![
        GlowQuad::gradient(0, 90, 38, 30, 6, 0x0030_0810, 0x30, 0x0008_1030, 0x50),
        GlowQuad::flat(0, 200, 40, 10, 4, 0x0010_2030, 0),
    ];
    let mut ramps = GlowRamps::default();
    // Rows 30..61 across the face (its rows 42..48), its run over rows
    // 40..44 across the face's top, a dark row at 50.
    ramps.open(1, 0, (95, 30, 30), ends(0, 0x40));
    for k in 1..30 {
        if k == 10 {
            assert!(ramps.extend_run(0, 4, ends(k, 0)));
        } else if !(11..14).contains(&k) {
            ramps.extend_last(
                u16::from(k == 20),
                ends(k, if k % 3 == 0 { 0x60 } else { 0 }),
            );
        }
    }
    // A ramp clear of the face.
    ramps.open(2, 0, (10, 10, 20), ends(40, 0));
    for k in 41..48 {
        ramps.extend_last(0, ends(k, 0));
    }
    let mut legacy = Vec::new();
    ramps.expand_into(&quads, &mut legacy);
    let (mut cut, mut pieces, mut piece_ramps) = (Vec::new(), Vec::new(), GlowRamps::default());
    let (mut q, mut r) = (quads.clone(), ramps.clone());
    assert!(shade_stream(
        (&mut q, &mut r),
        &bands,
        (&mut pieces, &mut piece_ramps, &mut cut)
    ));
    assert!(shade_quads(&mut legacy, &bands, (&mut pieces, &mut cut)));
    let mut got = Vec::new();
    r.expand_into(&q, &mut got);
    assert_eq!(got, legacy, "the ramps shade as their quads");
    assert_eq!(r.lit, r.count_lit());
    let kept: Vec<_> = r.items.iter().map(|k| (k.x, k.y, k.h)).collect();
    assert_eq!(
        kept,
        [(95, 30, 10), (95, 48, 13), (10, 10, 8)],
        "the rows above and below the face, and the ramp clear of it, stay ramps"
    );
    // A stream clear of the face stays as laid.
    let (mut q, mut r) = (quads[1..].to_vec(), GlowRamps::default());
    r.open(1, 0, (10, 10, 20), ends(40, 0));
    let laid = (q.clone(), r.clone());
    assert!(!shade_stream(
        (&mut q, &mut r),
        &bands,
        (&mut pieces, &mut piece_ramps, &mut cut)
    ));
    assert_eq!((q, r), laid);
}

/// A stream with ramps that the face reaches nothing of costs the shade
/// no copy, even where its lights lie inside the face's drawn rows'
/// bounds, beside the rows: it stays as laid, and the scratch the shade
/// rebuilds a stream in is left as it was.
#[test]
fn a_stream_the_face_reaches_nothing_of_is_not_copied() {
    let icon = (100, 112, 40, 50);
    let mut bands = Vec::new();
    drawn_rows(icon, Some(&face(icon, 0..12, false)), (0, 0), &mut bands);
    let end = aterm_render::glow_end(0x0020_3040, 0);
    // The face's top corners, each two rows tall: the rows there span
    // 103..109, the rows below them 102..110.
    let mut quads = vec![GlowQuad::flat(0, 102, 42, 1, 2, 0x0010_2030, 0)];
    let mut ramps = GlowRamps::default();
    ramps.open(1, 0, (109, 42, 1), (end, end));
    ramps.extend_last(0, (end, end));
    let bounds = bands_bounds(&bands);
    let (b, rb) = (quad_box(&quads[0]), ramp_box(&ramps.items[0]));
    for (x0, x1, y0, y1) in [b, rb] {
        assert!(
            x0 >= bounds.0 && x1 <= bounds.1 && y0 >= bounds.2 && y1 <= bounds.3,
            "inside the drawn rows' bounds {bounds:?}"
        );
    }
    let laid = (quads.clone(), ramps.clone());
    let sentinel = GlowQuad::flat(0, 1, 1, 1, 1, 0x0001_0203, 0);
    let mut held = GlowRamps::default();
    held.open(0, 7, (1, 1, 1), (end, 0));
    let (mut pieces, mut piece_ramps, mut cut) = (vec![sentinel], held.clone(), Vec::new());
    assert!(!shade_stream(
        (&mut quads, &mut ramps),
        &bands,
        (&mut pieces, &mut piece_ramps, &mut cut)
    ));
    assert_eq!((quads, ramps), laid, "the stream stays as laid");
    assert_eq!(pieces, [sentinel], "its quads are not copied");
    assert_eq!(piece_ramps, held, "nor its ramps");
    // Beside that quad, a ramp the face reaches through its rows, and one
    // it reaches only through its run (rows 40..44, the face's from 42):
    // each stream is shaded as its quads are.
    for run in [false, true] {
        let mut quads = vec![GlowQuad::flat(0, 102, 42, 1, 2, 0x0010_2030, 0)];
        let mut ramps = GlowRamps::default();
        if run {
            ramps.open_run(1, 0, (100, 40, 12), 4, (end, end));
        } else {
            ramps.open(1, 0, (100, 43, 12), (end, end));
            ramps.extend_last(0, (end, end));
        }
        let mut legacy = Vec::new();
        ramps.expand_into(&quads, &mut legacy);
        assert!(
            shade_stream(
                (&mut quads, &mut ramps),
                &bands,
                (&mut pieces, &mut piece_ramps, &mut cut)
            ),
            "run {run}: the ramp is reached"
        );
        assert!(shade_quads(&mut legacy, &bands, (&mut pieces, &mut cut)));
        let mut got = Vec::new();
        ramps.expand_into(&quads, &mut got);
        assert_eq!(got, legacy, "run {run}: shaded as its quads");
    }
}

/// A mirrored icon reads its texels mirrored: a face drawn only in the
/// art's left half is shaded in the box's right half.
#[test]
fn a_mirrored_icon_is_shaded_where_it_is_drawn() {
    let icon = (0, 12, 0, 10);
    let mut bands = Vec::new();
    drawn_rows(icon, Some(&face(icon, 0..6, false)), (0, 0), &mut bands);
    assert!(
        !bands.is_empty() && bands.iter().all(|b| b.1 <= 6),
        "unmirrored, the left half: {bands:?}"
    );
    drawn_rows(icon, Some(&face(icon, 0..6, true)), (0, 0), &mut bands);
    assert!(
        !bands.is_empty() && bands.iter().all(|b| b.0 >= 6),
        "mirrored, the right half: {bands:?}"
    );
}

/// A peeking icon is shaded only where it shows: its art's box reaches
/// past its visible window, and the drawn rows stop at the window's
/// edge, scaled to the art's whole box (here twice the art's size, and
/// placed by the grid's origin in the window).
#[test]
fn a_peeking_icon_is_shaded_only_where_it_shows() {
    let src = face((10, 34, 20, 40), 0..12, false);
    let (ox, oy) = (5, 3);
    // The visible window: the box's top half, in window pixels.
    let shown = (10 + ox, 34 + ox, 20 + oy, 30 + oy);
    let mut bands = Vec::new();
    drawn_rows(shown, Some(&src), (ox, oy), &mut bands);
    assert!(!bands.is_empty(), "the face shows in the window");
    assert!(
        bands.iter().all(|b| b.2 >= shown.2 && b.3 <= shown.3),
        "only the visible rows: {bands:?}"
    );
    // Art row 2 (the face's top) lands on box rows 4-5, window 27-28.
    assert_eq!(
        bands[0].2,
        20 + oy + 4,
        "scaled to the art's box: {bands:?}"
    );
    let mut whole = Vec::new();
    drawn_rows((15, 39, 23, 43), Some(&src), (ox, oy), &mut whole);
    assert!(
        whole.iter().map(|b| b.3).max() > bands.iter().map(|b| b.3).max(),
        "control: shown whole, the face's lower rows are drawn too"
    );
}

/// The pixels a sprite's stamp draws, `(x, y)` in window pixels (the
/// grid at the window's origin): the CPU engine's own spans, read
/// through its bloom source ([`aterm_render::fx_bloom_quads`]).
fn stamped(sprite: &FreeSprite) -> std::collections::HashSet<(i32, i32)> {
    let mut layers = Vec::new();
    aterm_render::sprites::SpriteLayers::new(7).publish(std::slice::from_ref(sprite), &mut layers);
    for l in &mut layers {
        l.bloom = true;
    }
    let f = aterm_render::FxFrame {
        w: 256,
        h: 256,
        pad: 0,
        grid_top: 0,
        cell_h: 16,
        rows: 16,
    };
    let mut quads = Vec::new();
    aterm_render::fx_bloom_quads(
        &f,
        &layers,
        &[],
        &mut aterm_render::FxSlicer::default(),
        &mut aterm_render::SpriteCols::default(),
        &mut quads,
    );
    let mut px = std::collections::HashSet::new();
    for q in quads.iter().filter(|q| q.alpha > 0) {
        let (x, y) = (i32::from(q.x), i32::from(q.y));
        for yy in y..y + i32::from(q.h) {
            for xx in x..x + i32::from(q.w) {
                px.insert((xx, yy));
            }
        }
    }
    px
}

/// A SCALED SPRITE'S DRAWN ROWS ARE THE PIXELS ITS STAMP DRAWS (looks7
/// round 2): the companion's art drawn at half its size, the glass
/// yield's rule 1 cuts to these rows, and read a texel off them, or from
/// the icon's half-opaque edge, it misses the paw and the chin's
/// anti-aliased edge the stamp draws. Over
/// arts scaled down, up and unevenly, plain and mirrored, with
/// translucent texels among the opaque, every pixel the CPU engine
/// stamps lies in a drawn row, and every row's span starts and ends on a
/// stamped pixel. Negative controls: read at the texel centres, or
/// counted from the icon's edge, a stamped pixel lies outside the rows.
#[test]
fn a_scaled_sprite_s_drawn_rows_are_the_pixels_its_stamp_draws() {
    use aterm_core::render::PxRect;
    use aterm_render::sprites::FreeZ;
    // A speckled art: about a third of its texels opaque, the rest clear.
    let art = |w: u16, h: u16| {
        let mut rgba = vec![0u8; usize::from(w) * usize::from(h) * 4];
        let mut seed = 0x9E37_79B9u32;
        for t in rgba.as_chunks_mut::<4>().0 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            if seed >> 30 == 0 || (seed >> 28) & 3 == 1 {
                *t = [0xE0, 0xD8, 0xC8, 0xFF];
            } else if (seed >> 26) & 7 == 5 {
                *t = [0xE0, 0xD8, 0xC8, 0x40];
            }
        }
        Arc::new(FxArt::new(w, h, FxTexels::Rgba8Straight, rgba).expect("an art"))
    };
    let centred = |l: i32, a: i32, d: i32, flip: bool| {
        let m = if flip { d - 1 - l } else { l };
        (2 * m + 1) * a / (2 * d)
    };
    let (mut missed_centred, mut missed_edge) = (0usize, 0usize);
    for (aw, ah, dw, dh) in [
        (40, 20, 20, 10),
        (34, 26, 21, 13),
        (12, 10, 30, 23),
        (9, 7, 9, 7),
    ] {
        for flip_x in [false, true] {
            let sprite = FreeSprite {
                x: 17,
                y: 11,
                w: dw,
                h: dh,
                art: art(aw, ah),
                clip: PxRect::EVERYWHERE,
                tint: 0x00FF_FFFF,
                alpha: 255,
                flip_x,
                z: FreeZ::UnderText,
            };
            let drawn = stamped(&sprite);
            assert!(!drawn.is_empty(), "{aw}x{ah} at {dw}x{dh}: the stamp draws");
            let clip = (0, 256, 0, 256);
            let mut bands = Vec::new();
            sprite_rows(&sprite, clip, (0, 0), &mut bands);
            let held = lit(&bands);
            for &px in &drawn {
                assert!(
                    held.contains_key(&px),
                    "{aw}x{ah} at {dw}x{dh}, flip {flip_x}: stamped {px:?} lies in no drawn row: {bands:?}"
                );
            }
            for &(x0, x1, y0, y1) in &bands {
                for y in y0..y1 {
                    assert!(
                        drawn.contains(&(x0, y)) && drawn.contains(&(x1 - 1, y)),
                        "{aw}x{ah} at {dw}x{dh}, flip {flip_x}: row {y} spans {x0}..{x1} past what the stamp drew"
                    );
                }
            }
            let src = IconSrc {
                art: Arc::clone(&sprite.art),
                dest: (17, 17 + i32::from(dw), 11, 11 + i32::from(dh)),
                flip_x,
            };
            drawn_rows_by(clip, Some(&src), (0, 0), &mut bands, (centred, 1));
            let held = lit(&bands);
            missed_centred += drawn.iter().filter(|px| !held.contains_key(px)).count();
            drawn_rows_by(
                clip,
                Some(&src),
                (0, 0),
                &mut bands,
                (stamp_texel, ICON_ALPHA_EDGE),
            );
            let held = lit(&bands);
            missed_edge += drawn.iter().filter(|px| !held.contains_key(px)).count();
        }
    }
    assert!(
        missed_centred > 0,
        "control: read at the texel centres, a stamped pixel lies outside the rows"
    );
    assert!(
        missed_edge > 0,
        "control: counted from the icon's edge, a stamped pixel lies outside the rows"
    );
}

/// A star by an icon goes whole, and only the star: a piece of the wash
/// inside the star's box (a train cut row by row) keeps its light, where
/// the star's box alone took it and left a dark notch. A star clear of
/// the icon stays.
#[test]
fn a_star_by_an_icon_goes_whole_and_alone() {
    let quad = |x: u16, y: u16, w: u16, h: u16, color: u32| GlowQuad {
        row: 0,
        x,
        y,
        w,
        h,
        color,
        alpha: 0,
        color2: color,
        alpha2: 0,
    };
    let star = [
        quad(40, 50, 11, 1, 0x00FF_C0FF),
        quad(45, 45, 1, 11, 0x00FF_C0FF),
    ];
    let wash = quad(41, 47, 6, 1, 0x0030_1060);
    let far = [
        quad(140, 50, 11, 1, 0x00FF_C0FF),
        quad(145, 45, 1, 11, 0x00FF_C0FF),
    ];
    let mut keys = Vec::new();
    let near = rk::stardust::SkyMark::of((45, 50), 6, (&star[..], &[]), &mut keys)
        .expect("the star painted");
    let away = rk::stardust::SkyMark::of((145, 50), 6, (&far[..], &[]), &mut keys)
        .expect("the far star painted");
    let in_box = |q: &GlowQuad| {
        let b = quad_box(q);
        b.0 >= near.rect.0 && b.1 <= near.rect.1 && b.2 >= near.rect.2 && b.3 <= near.rect.3
    };
    assert!(
        in_box(&wash),
        "fixture: the wash's piece lies in the star's box"
    );
    let mut over: Vec<GlowQuad> = star.iter().chain(&far).copied().chain([wash]).collect();
    let (mut under, mut halos) = (Vec::new(), Vec::new());
    let (mut over_ramps, mut under_ramps) = (GlowRamps::default(), GlowRamps::default());
    // An icon just right of the near star, its arm reaching it.
    let icon = [(48, 70, 30, 60)];
    assert!(lift_stars(
        (&[near, away], &keys),
        &icon,
        (0, 0),
        (
            (&mut over, &mut over_ramps),
            (&mut under, &mut under_ramps),
            &mut halos
        )
    ));
    assert!(
        star.iter().all(|q| !over.contains(q)),
        "the star by the icon goes whole: {over:?}"
    );
    assert!(
        over.contains(&wash),
        "the wash's piece in its box stays lit"
    );
    assert!(
        far.iter().all(|q| over.contains(q)),
        "a star clear of the icon stays"
    );
}

/// A split quad continues its gradient: the pieces' colours at their
/// columns are the whole quad's at the same columns.
#[test]
fn a_split_quad_keeps_its_gradient() {
    let q = GlowQuad {
        row: 2,
        x: 90,
        y: 50,
        w: 50,
        h: 20,
        color: 0x0000_0000,
        alpha: 0,
        color2: 0x00C8_6432,
        alpha2: 200,
    };
    let mut quads = vec![q];
    let (mut pieces, mut cut) = (Vec::new(), Vec::new());
    assert!(shade_quads(
        &mut quads,
        &[(100, 130, 30, 60)],
        (&mut pieces, &mut cut)
    ));
    assert!(quads.len() >= 2, "the quad split: {quads:?}");
    let at = |q: &GlowQuad, x: i32| {
        let k = x - i32::from(q.x);
        lerp_rgb(q.color, q.color2, k, i32::from(q.w))
    };
    for p in &quads {
        let x = i32::from(p.x);
        let whole = at(&q, x);
        let piece = at(p, x);
        let gap = [16u32, 8, 0]
            .iter()
            .map(|&sh| (((whole >> sh) & 0xff) as i32 - ((piece >> sh) & 0xff) as i32).abs())
            .max()
            .unwrap_or(0);
        assert!(
            gap <= 1,
            "piece {p:?} at {x}: {piece:06x} against {whole:06x}"
        );
        assert_eq!(p.row, 2, "a piece keeps its row band");
    }
}
