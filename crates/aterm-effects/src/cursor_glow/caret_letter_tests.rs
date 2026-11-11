// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A light the caret's cell lies in is cut round the cell and nowhere else: its pieces
/// cover the light less the cell, none reaches into it, each piece keeps the light's own
/// gradient where it starts, and the stream keeps its order. Control: a light clear of the
/// cell is left as it was.
#[test]
fn the_caret_s_cell_is_punched_out_of_the_light_and_nothing_else() {
    let wide = GlowQuad::gradient(2, 10, 50, 60, 30, 0x0010_2030, 0, 0x0070_8090, 0);
    let clear = GlowQuad::flat(1, 0, 0, 8, 8, 0x0040_4040, 0);
    let mut out = vec![clear, wide, clear];
    let mut scratch = Vec::new();
    let hole = (30, 54, 44, 80);
    assert_eq!(punch_quads(&mut out, &mut [], &mut scratch, hole), 1);
    assert_eq!(out.first(), Some(&clear));
    assert_eq!(out.last(), Some(&clear));
    let pieces = &out[1..out.len() - 1];
    let area = |q: &GlowQuad| i32::from(q.w) * i32::from(q.h);
    assert_eq!(pieces.iter().map(area).sum::<i32>(), 60 * 30 - 14 * 26);
    for p in pieces {
        let (x0, y0) = (i32::from(p.x), i32::from(p.y));
        let (x1, y1) = (x0 + i32::from(p.w), y0 + i32::from(p.h));
        assert!(
            x1 <= hole.0 || x0 >= hole.2 || y1 <= hole.1 || y0 >= hole.3,
            "{p:?} reaches into the caret's cell"
        );
        let at = (x0 - 10) as f32 / 60.0;
        assert_eq!(
            p.color,
            crate::effect_util::lerp_rgb(wide.color, wide.color2, at),
            "{p:?} starts on the light's own gradient"
        );
        assert_eq!(p.row, wide.row);
    }
    let mut alone = vec![clear];
    assert_eq!(punch_quads(&mut alone, &mut [], &mut scratch, hole), 0);
    assert_eq!(alone, vec![clear]);
}

/// The stream's ramps keep their places among its quads through the punch: each stays
/// before the first piece of the quad it stood before, and after every quad where it
/// stood after them all. Unmoved, a ramp drawn after the punched light was drawn among
/// its pieces, or before it.
#[test]
fn the_punch_keeps_every_ramp_in_its_place() {
    let wide = GlowQuad::gradient(2, 10, 50, 60, 30, 0x0010_2030, 0, 0x0070_8090, 0);
    let clear = GlowQuad::flat(1, 0, 0, 8, 8, 0x0040_4040, 0);
    let ramp = |at: u32| GlowRamp {
        at,
        row: 0,
        x: 200,
        y: 0,
        w: 4,
        h: 4,
        join: 0,
        joined: 0,
        ink: 0,
    };
    let mut out = vec![clear, wide, clear];
    let mut ramps = [ramp(0), ramp(1), ramp(2), ramp(3)];
    let mut scratch = Vec::new();
    assert_eq!(
        punch_quads(&mut out, &mut ramps, &mut scratch, (30, 54, 44, 80)),
        1
    );
    let pieces = out.len() - 2;
    assert!(pieces > 1, "fixture: the light is cut into pieces");
    let ats: Vec<u32> = ramps.iter().map(|r| r.at).collect();
    assert_eq!(ats, vec![0, 1, 1 + pieces as u32, out.len() as u32]);
    assert!(!ramp_meets(&ramps[0], (30, 54, 44, 80)));
    assert!(ramp_meets(&ramps[0], (198, 2, 202, 6)));
}
