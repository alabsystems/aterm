// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Lumen on paper is a tracer, not Off: the trail gallery's typing scene replayed through the
// shipping effects pipeline and the real renderer, the host's post passes on, each kept frame
// read against its clean twin.

use crate::trail_letters_read::{
    DEFAULT_LIGHT, Ev, GITHUB_LIGHT, Kept, Look, Replay, TYPING_TEXT, typed,
};
use aterm_effects::cursor_fx::TrailColor;
use aterm_effects::pipeline::EffectsPipeline;

/// A channel off its clean twin's by more than this is the effects' (`trail-contrast`'s
/// `diff_vs_clean`).
const DIFF_DELTA: u32 = 12;

/// The last key of the typing scene, ms.
const LAST_KEY: u64 = 1_000 + 60 * (TYPING_TEXT.len() as u64 - 1);

/// The cells behind the caret the tracer is read over: from the second cell back (past the
/// momentum glow's reach, which Off draws as well) to the eighth.
const BEHIND: std::ops::Range<usize> = 2..9;

/// The pixels of the caret's row, in [`BEHIND`]'s cells, the effects changed in `k`.
fn tracer_px(k: &Kept) -> usize {
    let (cw, ch) = k.cell;
    let (row, col) = k.caret;
    let y0 = k.top + row * ch;
    let x1 = k.left + col.saturating_sub(BEHIND.start - 1) * cw;
    let x0 = k.left + col.saturating_sub(BEHIND.end - 1) * cw;
    (y0..y0 + ch)
        .flat_map(|y| (x0..x1).map(move |x| y * k.w + x))
        .filter(|&i| {
            let (f, c) = (k.px[i], k.clean[i]);
            [16u32, 8, 0]
                .iter()
                .any(|&sh| ((f >> sh) & 0xff).abs_diff((c >> sh) & 0xff) > DIFF_DELTA)
        })
        .count()
}

/// The typing scene under `style` on `look`, the post passes on, `controls` applied: the
/// frame `after` ms past the last key.
fn typed_after(
    style: &str,
    enabled: bool,
    look: Look,
    after: u64,
    controls: impl Fn(&mut EffectsPipeline),
) -> Kept {
    let mut r = Replay::new(style, look, TrailColor::Automatic);
    if !enabled {
        r.fx.set_cursor_glow(
            false,
            style,
            TrailColor::Automatic,
            None,
            260,
            24,
            1.0,
            0.6,
            true,
            look.cursor,
        );
    }
    r.post = true;
    controls(&mut r.fx);
    let at = (LAST_KEY + after).div_ceil(16) * 16;
    r.keep = vec![at];
    let mut ev = vec![(200, Ev::Out(b"\x1b[2J\x1b[5H$ ".to_vec()))];
    ev.extend(typed(TYPING_TEXT, 1_000, 60));
    let _ = r.run(&ev, 600);
    r.kept.pop().expect("fixture: the frame is kept")
}

/// Lumen on paper keeps a tracer behind the caret when the typing stops. Its sparks lived
/// as briefly as the dark face's (about a key and a tenth of a second), so its paper tracer
/// was two cells long and gone 120 ms after the last key: the gallery's `t120` and `t470`
/// frames of Lumen on paper were Off's, pixel for pixel (composed r2's three judges, O9(b)).
/// 120 ms after the last key it lights at least a cell and a half's worth of the caret's row
/// from two to eight cells behind the caret, where Off lights nothing. Control:
/// `set_lumen_paper_tracer` off gives it the dark face's brief sparks again, and it lights
/// under half a cell's worth there.
#[test]
fn lumen_on_paper_keeps_a_tracer_when_the_typing_stops() {
    for look in [GITHUB_LIGHT, DEFAULT_LIGHT] {
        let off = tracer_px(&typed_after("off", false, look, 120, |_| {}));
        let lumen = typed_after("lumen", true, look, 120, |_| {});
        let (cw, ch) = lumen.cell;
        let cell = cw * ch;
        let floor = cell * 3 / 2;
        let lit = tracer_px(&lumen);
        let brief = tracer_px(&typed_after("lumen", true, look, 120, |fx| {
            fx.set_lumen_paper_tracer(false)
        }));
        assert_eq!(
            off, 0,
            "{}: fixture: Off lights nothing behind the caret",
            look.name
        );
        assert!(
            lit >= floor,
            "{}: 120 ms after the last key paper Lumen lights {lit} px behind the caret, \
             under a cell and a half's {floor}",
            look.name
        );
        assert!(
            brief * 2 < cell,
            "{}: control: with the dark face's brief sparks paper Lumen lights {brief} px \
             behind the caret, not under half a cell's {cell}",
            look.name
        );
    }
}
