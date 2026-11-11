// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

/// A run of `n` square glyphs 10 units wide, with hidden joiners between.
fn joined(n: u32) -> Vec<(u32, i32, i32, i32)> {
    let mut glyphs = Vec::new();
    for i in 0..n {
        if i > 0 {
            glyphs.push((3, 0, 0, 0)); // a hidden ZWJ: no colour, no advance
        }
        glyphs.push((100 + i, 10, 0, 0));
    }
    glyphs
}

fn colour(gid: u16) -> bool {
    gid >= 100
}

#[test]
fn a_run_keeps_its_colour_glyphs_and_skips_hidden_ones() {
    let run = layout_run(joined(3), colour, false).expect("three colour glyphs");
    let at: Vec<(u16, i32)> = run.glyphs.iter().map(|g| (g.gid, g.x)).collect();
    assert_eq!(at, [(100, 0), (101, 10), (102, 20)]);
    assert_eq!((run.width, run.em), (30, 10));
}

#[test]
fn a_run_is_refused_unless_it_is_all_colour_and_two_to_eight_glyphs() {
    assert!(layout_run(joined(1), colour, false).is_none());
    assert!(layout_run(joined(MAX_RUN_GLYPHS as u32), colour, false).is_some());
    assert!(layout_run(joined(MAX_RUN_GLYPHS as u32 + 1), colour, false).is_none());
    let mut text = joined(2);
    text.push((7, 10, 0, 0));
    assert!(layout_run(text, colour, false).is_none());
    assert!(layout_run([(0, 10, 0, 0), (100, 10, 0, 0)], |_| true, false).is_none());
    assert!(layout_run([(100, 10, 0, 0), (101, 0, 0, 0)], colour, false).is_none());
    let marked = layout_run([(100, 10, 0, 0), (101, 0, 0, 0)], colour, true)
        .expect("a member placed by its pen");
    let at: Vec<(u16, i32, i32)> = marked
        .glyphs
        .iter()
        .map(|g| (g.gid, g.x, g.advance))
        .collect();
    assert_eq!(at, [(100, 0, 10), (101, 10, 0)]);
    assert!(layout_run([(100, 10, 0, 0), (101, -1, 0, 0)], colour, true).is_none());
}

/// Two opaque squares fitted into a 2:1 box land side by side within the box.
#[test]
fn a_composite_places_each_glyph_in_its_slot_within_the_box() {
    let run = layout_run(joined(2), colour, false).unwrap();
    let solid = |gid: u16, w: usize, h: usize| {
        let v = if gid == 100 { 10 } else { 200 };
        Some(([v, v, v, 255].repeat(w * h), w, h))
    };
    let (rgba, w, h) = composite(&run, 20, 10, solid).unwrap();
    assert_eq!((w, h), (20, 10));
    let texel = |x: usize, y: usize| rgba[(y * w + x) * 4];
    assert_eq!(
        (texel(0, 0), texel(9, 9)),
        (10, 10),
        "left half: first glyph"
    );
    assert_eq!(
        (texel(10, 0), texel(19, 9)),
        (200, 200),
        "right half: second"
    );

    let run = layout_run(joined(3), colour, false).unwrap();
    let (_, w, h) = composite(&run, 20, 10, solid).unwrap();
    assert_eq!((w, h), (20, 7));

    assert!(
        composite(&run, 20, 10, |_, _, _| None).is_none(),
        "nothing drew"
    );
}

#[test]
fn over_blends_straight_alpha_and_clips() {
    let mut dst = vec![0, 0, 255, 255];
    over((&mut dst, 1, 1), (&[255, 0, 0, 128], 1, 1), 0, 0);
    assert_eq!(dst, [128, 0, 127, 255]);
    let mut dst = vec![0u8; 2 * 4];
    over(
        (&mut dst, 2, 1),
        (&[9, 9, 9, 255, 7, 7, 7, 255], 2, 1),
        1,
        0,
    );
    assert_eq!(dst, [0, 0, 0, 0, 9, 9, 9, 255]);
    over((&mut dst, 2, 1), (&[5, 5, 5, 255], 1, 1), -1, 0);
    assert_eq!(dst, [0, 0, 0, 0, 9, 9, 9, 255]);
}

/// Run ids are never reused and stay live only while referenced by the memo.
#[test]
fn runs_live_exactly_as_long_as_the_memo_names_them() {
    let mut shapes = ClusterShapes::default();
    let run = || layout_run(joined(2), colour, false).unwrap();
    let a = shapes.add_run(run()).unwrap();
    shapes.insert("a", Some(ClusterShape::Run(a)));
    shapes.insert("g", Some(ClusterShape::Glyph { face: 0, gid: 5 }));
    let b = shapes.add_run(run()).unwrap();
    assert_ne!(a, b);
    shapes.insert("b", Some(ClusterShape::Run(b)));

    shapes.rotate();
    assert!(
        shapes.run(a).is_some() && shapes.run(b).is_some(),
        "prev names both"
    );
    let (shape, prev) = shapes.get("a").unwrap();
    assert!(prev);
    shapes.insert("a", shape);
    shapes.rotate();
    assert!(shapes.run(a).is_some(), "a hot cluster keeps its run");
    assert!(shapes.run(b).is_none(), "a dropped cluster frees its run");
    assert!(shapes.get("b").is_none());

    shapes.clear();
    assert!(shapes.is_empty());
    let c = shapes.add_run(run()).unwrap();
    assert!(c > b, "a clear does not recycle ids");

    shapes.next_run = u32::MAX;
    assert!(shapes.add_run(run()).is_none(), "exhausted ids refuse");
    assert!(shapes.add_run(run()).is_none());
}
