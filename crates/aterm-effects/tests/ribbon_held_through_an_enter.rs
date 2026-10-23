// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A LINE AN ENTER SCROLLS UP KEEPS ITS LIGHT IN ITS ROW (the 2026-10-23 `enter-bottom` capture:
//! the echo's core lay across the tops of `the quick brown fox` printed under it, `b`, `i` and
//! `k` at 1.5:1 by the lab's local judge). Typed on the bottom row, a command's light is laid
//! inside its row (the pane's last row has no row under it). Enter scrolls it up over the line it
//! prints, and from the first frame after it, through the scroll's frame (which clears the
//! probe) and the retract, no light lies on the printed line's ascenders. Driven at the host
//! seam (`trail_host`, with the caret row's flanks sent as `app_render.rs` sends them). Control:
//! an Enter that prints nothing leaves a blank row under the echo, and the same reading sees the
//! free line's light reach into it.

use crate::trail_host::{Pace, PacedHost};
use std::time::Duration;

const ROWS: usize = 14;
const COLS: usize = 72;
const TYPED: &str = "echo the quick brown fox";
const PRINTED: &str = "the quick brown fox";

/// The light (pixels × opacity) the bed's quads lay on the top `share` of `row`'s band over
/// columns `c0..c1`.
fn on_top_of(h: &PacedHost, row: u16, share: f32, c0: usize, c1: usize) -> f32 {
    let (cw, ch) = (h.g.cw as f32, h.g.ch as f32);
    let y0 = f32::from(h.g.origin_y) + f32::from(row) * ch;
    let y1 = y0 + share * ch;
    let (x0, x1) = (c0 as f32 * cw, c1 as f32 * cw);
    h.glow
        .under_quads()
        .iter()
        .filter(|q| q.w > 0 && q.alpha > 0)
        .map(|q| {
            let (qx0, qy0) = (f32::from(q.x), f32::from(q.y));
            let (qx1, qy1) = (qx0 + f32::from(q.w), qy0 + f32::from(q.h));
            let w = (qx1.min(x1) - qx0.max(x0)).max(0.0);
            let hgt = (qy1.min(y1) - qy0.max(y0)).max(0.0);
            w * hgt * f32::from(q.alpha) / 255.0
        })
        .sum()
}

/// `seq 1 40` on the screen, `$ ` on the bottom row, `TYPED` at 60 ms a key, Enter 90 ms later,
/// then `printed` (if any) and a new prompt. Each frame from Enter on for 700 ms, `(ms after
/// Enter, the light on the top third of the row under the echo, the light in the echo's row)`.
fn enter_at_the_bottom(printed: &str) -> Vec<(u64, f32, f32)> {
    let mut h = PacedHost::new(ROWS, COLS, Pace::Flat(16), false);
    h.c.flanks = true;
    let mut seq = String::from("\x1b[2J\x1b[H");
    for i in 1..=40 {
        seq.push_str(&format!("{i}\r\n"));
    }
    seq.push_str("$ ");
    h.land(seq.as_bytes());
    h.advance(400);
    h.type_plain(TYPED, 60);
    h.advance(88);
    let now = h.now;
    h.glow.note_return(now);
    h.advance(2);
    h.land(b"\r\n");
    let enter = h.t();
    h.advance(4);
    let (tail, echo) = if printed.is_empty() {
        ("$ ".to_string(), ROWS as u16 - 2)
    } else {
        (format!("{printed}\r\n$ "), ROWS as u16 - 3)
    };
    h.land(tail.as_bytes());
    let mut out = Vec::new();
    let end = h.now + Duration::from_millis(700);
    while h.now < end {
        out.push((
            h.t() - enter,
            on_top_of(&h, echo + 1, 0.35, 0, TYPED.len() + 2),
            on_top_of(&h, echo, 1.0, 2, TYPED.len() + 2),
        ));
        h.advance(16);
    }
    out
}

#[test]
fn a_line_an_enter_scrolls_up_keeps_its_light_in_its_row() {
    let frames = enter_at_the_bottom(PRINTED);
    let (_, _, first_own) = frames[0];
    assert!(
        first_own > 100.0,
        "premise: the echo's own row is lit when Enter lands ({first_own:.1})"
    );
    for &(ms, ascenders, _) in &frames {
        assert!(
            ascenders < 0.5,
            "+{ms} ms after Enter: the echo lays {ascenders:.1} px of light on the printed \
             line's ascenders"
        );
    }

    let free = enter_at_the_bottom("");
    let reach = free.iter().map(|f| f.1).fold(0.0f32, f32::max);
    assert!(
        reach > 20.0,
        "control: over a blank row the free line's light reaches into it ({reach:.1} px at most); \
         the reading above sees nothing"
    );
}
