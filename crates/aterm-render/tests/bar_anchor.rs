// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A bar caret's lights sit on the bar: every caret-anchored light reads `GlowConfig::head_dx`.

use crate::trail_letters_read::{
    DARK, Ev, GITHUB_LIGHT, Kept, Look, Replay, STYLES, TYPING_TEXT, typed,
};
use aterm_effects::cursor_fx::TrailColor;

/// The typing scene with a steady bar caret (`\e[6 q`, else a steady block, `\e[2 q`), the
/// bar anchor on or off (`EffectsPipeline::set_bar_anchor`), kept every 48 ms from the first
/// key.
fn typed_bar(style: &str, look: Look, bar: bool, anchored: bool) -> Vec<Kept> {
    typed_bar_band(style, look, bar, anchored, true)
}

/// [`typed_bar`], the ribbon's band under the caret ending past a bar or not
/// (`EffectsPipeline::set_ribbon_bar_band`).
fn typed_bar_band(style: &str, look: Look, bar: bool, anchored: bool, band: bool) -> Vec<Kept> {
    let mut r = Replay::new(style, look, TrailColor::Automatic);
    r.fx.set_bar_anchor(anchored);
    r.fx.set_ribbon_bar_band(band);
    let end = 1_000 + 60 * TYPING_TEXT.len() as u64 + 600;
    r.keep = (1_008..end).step_by(48).collect();
    let shape: &[u8] = if bar { b"\x1b[6 q" } else { b"\x1b[2 q" };
    let mut first = shape.to_vec();
    first.extend_from_slice(b"\x1b[2J\x1b[5H$ ");
    let mut ev = vec![(200, Ev::Out(first))];
    ev.extend(typed(TYPING_TEXT, 1_000, 60));
    let _ = r.run(&ev, 600);
    std::mem::take(&mut r.kept)
}

/// The ribbon styles whose band runs under the caret's cell (`Ribbon::plan_run`'s attach).
const RIBBONS: [&str; 4] = [
    "tall rainbow + cat",
    "tall rainbow + dog",
    "flat rainbow + cat",
    "tall rainbow",
];

/// The light the effects add (frame over clean twin, summed channel steps over 12) across
/// the caret's row band, in the columns `[x0 + a, x0 + b)` of the caret's cell's left edge `x0`.
fn light_in(k: &Kept, (a, b): (i64, i64)) -> f64 {
    let (cw, ch) = k.cell;
    let x0 = (k.left + k.caret.1 * cw) as i64;
    let y0 = k.top + k.caret.0 * ch;
    let mut sum = 0.0;
    for y in y0..y0 + ch {
        for x in (x0 + a).max(0)..(x0 + b).min(k.w as i64) {
            let i = y * k.w + x as usize;
            let (f, c) = (k.px[i], k.clean[i]);
            let add: i32 = [16u32, 8, 0]
                .iter()
                .map(|&sh| (((f >> sh) & 0xff) as i32 - ((c >> sh) & 0xff) as i32).abs())
                .sum();
            if add > 12 {
                sum += f64::from(add);
            }
        }
    }
    sum
}

/// A BAR CARET'S LIGHTS SIT ON THE BAR (composed r0's judges, must_fix 2; S1(a)): behind a
/// bar (`\e[6 q`, `cursor_style = "bar"`) every caret-seated light sat at the block's centre,
/// so the head glow, the halos, the crown and the momentum halo stood 0.42 of a cell right of
/// the bar and ran on into the cell the next letter lands in (Laser read as a green bar with a
/// white-violet blob beside it). Each now reads `GlowConfig::head_dx`
/// (`GlowConfig::caret_center`, the crown, the momentum halo's seat, Classic's landing ring),
/// as Classic's streak and the comet's caret bridge always did. Every style on dark and
/// GitHub Light, the steady typing frames: the light laid in the cell past the bar is at most
/// 0.8 of what the same frames lay with every light at the block's centre, and for the four
/// ribbon styles whose band runs under the caret ([`RIBBONS`]) so is the light laid in the
/// bar's own cell right of the bar. (The cell past the bar alone missed that band,
/// `Ribbon::plan_run`'s attach, which crossed the bar's whole cell: a coloured block beside the
/// bar at 0.97 of its block-centred light, composed r1's judges, O4(c); measured now 0.42-0.68.
/// Every other style's head is a glow centred on the bar, spread to both sides.) On a block
/// nothing moves: the same frames with and without the anchor are the same pixels. Negative
/// controls: `EffectsPipeline::set_bar_anchor(false)` alone, the frames the law is read against
/// (where the law is reverted the two readings are equal, a ratio of 1); and
/// `EffectsPipeline::set_ribbon_bar_band(false)` alone (the band crosses the bar's cell again),
/// under which every ribbon style on dark lays at least 0.9 of its block-centred light in the
/// bar's own cell.
#[test]
fn a_bar_caret_s_lights_sit_on_the_bar() {
    for look in [DARK, GITHUB_LIGHT] {
        for style in STYLES {
            // The light in the cell past the bar, and in the bar's own cell right of the bar
            // (the bar is `(cell_w / 8).max(2)` px; one more for its antialiased edge).
            let past = |anchored: bool| -> (f64, f64) {
                let ks = typed_bar(style, look, true, anchored);
                let steady: Vec<&Kept> = ks
                    .iter()
                    .filter(|k| (1_584..=2_400).contains(&k.ms))
                    .collect();
                assert!(
                    steady.len() >= 10,
                    "fixture: {style} keeps its typing frames"
                );
                let cw = steady[0].cell.0 as i64;
                let bar = (cw / 8).max(2) + 1;
                (
                    steady.iter().map(|k| light_in(k, (cw, 2 * cw))).sum(),
                    steady.iter().map(|k| light_in(k, (bar, cw))).sum(),
                )
            };
            let ((on, on_in), (off, off_in)) = (past(true), past(false));
            assert!(
                off > 0.0 && off_in > 0.0,
                "fixture: {style} on {} lights past the bar",
                look.name
            );
            assert!(
                on <= 0.8 * off && (!RIBBONS.contains(&style) || on_in <= 0.8 * off_in),
                "{style} on {}: the light past a bar is {:.2} of its block-centred light, and \
                 in the bar's own cell {:.2}",
                look.name,
                on / off,
                on_in / off_in
            );
        }
    }
    for style in RIBBONS {
        let own = |anchored: bool, band: bool| -> f64 {
            typed_bar_band(style, DARK, true, anchored, band)
                .iter()
                .filter(|k| (1_584..=2_400).contains(&k.ms))
                .map(|k| {
                    let cw = k.cell.0 as i64;
                    light_in(k, ((cw / 8).max(2) + 1, cw))
                })
                .sum()
        };
        let (wide, off) = (own(true, false), own(false, true));
        assert!(
            wide >= 0.9 * off,
            "negative control: {style}'s band crossing the bar's cell lays {:.2} of its \
             block-centred light there",
            wide / off
        );
    }
    for style in ["laser", "classic", "tall rainbow + cat"] {
        let pixels = |anchored: bool| -> Vec<Vec<u32>> {
            typed_bar(style, DARK, false, anchored)
                .into_iter()
                .map(|k| k.px)
                .collect()
        };
        assert!(
            pixels(true) == pixels(false),
            "{style}: a block caret's frames do not depend on the bar anchor"
        );
    }
}
