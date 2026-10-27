// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests that an echoed line scrolled up by Enter keeps its glow within its row.

use crate::common::on_ground;
use crate::trail_host::{Pace, PacedHost, light_grounds, on_every_ground};
use std::time::Duration;

const ROWS: usize = 14;
const COLS: usize = 72;
const TYPED: &str = "echo the quick brown fox";
const PRINTED: &str = "the quick brown fox";

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

/// One frame after Enter: ms since it, the light on the printed line's
/// ascenders, the light in the echo's own row, and whether the Return's
/// meteor still flies.
type Read = (u64, f32, f32, bool);

/// The frames after Enter, and whether the host stands on a dark ground.
fn enter_at_the_bottom(printed: &str) -> (Vec<Read>, bool) {
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
            h.glow.v2_status().is_some_and(|s| s.meteors > 0),
        ));
        h.advance(16);
    }
    (out, h.cfg.dark_theme)
}

fn premise(frames: &[Read]) {
    let (_, _, first_own, _) = frames[0];
    assert!(
        first_own > 100.0,
        "premise: the echo's own row is lit when Enter lands ({first_own:.1})"
    );
}

fn control() {
    let (free, _) = enter_at_the_bottom("");
    let reach = free.iter().map(|f| f.1).fold(0.0f32, f32::max);
    assert!(
        reach > 20.0,
        "control: over a blank row the free line's light reaches into it ({reach:.1} px at most); \
         the reading above sees nothing"
    );
}

/// On a light ground the Return's meteor's frames are the stated limit below.
#[test]
fn a_line_an_enter_scrolls_up_keeps_its_light_in_its_row() {
    on_every_ground(|| {
        let (frames, dark) = enter_at_the_bottom(PRINTED);
        premise(&frames);
        for &(ms, ascenders, _, meteor) in &frames {
            if meteor && !dark {
                continue;
            }
            assert!(
                ascenders < 0.5,
                "+{ms} ms after Enter: the echo lays {ascenders:.1} px of light on the printed \
                 line's ascenders"
            );
        }
        control();
    });
}

/// The limit, on every light ground: main's paper arm draws the Return's
/// meteor train as Over ink in the under stream (meteor.rs `draw_train`) on a
/// rail `LIGHT_RAIL_CH` below its row's centre, and the row hold
/// (`Ribbon::plan_over`) holds only the ribbon, so while the meteor flies the
/// echo lays ink on the printed line's ascenders (84.8 px at +4 ms after Enter
/// on each light built-in). Dark's Add train stays in its row.
#[test]
fn the_limit_on_light_the_returns_meteor_lays_ink_on_the_printed_lines_ascenders() {
    for g in light_grounds() {
        on_ground(g, || {
            let (frames, dark) = enter_at_the_bottom(PRINTED);
            assert!(!dark, "premise: {} is a light ground", g.name);
            premise(&frames);
            let worst = frames
                .iter()
                .filter(|f| f.3)
                .map(|f| f.1)
                .fold(0.0f32, f32::max);
            assert!(
                worst >= 0.5,
                "on {}: a stated LIMIT now holds — restate the law: while the meteor flies the \
                 echo lays at most {worst:.1} px on the printed line's ascenders",
                g.name
            );
            control();
        });
    }
}
