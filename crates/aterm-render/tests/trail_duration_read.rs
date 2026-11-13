// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Trail duration (`cursor_trail_ms`) is a knob a typist sees: the trail gallery's typing scene
// replayed through the shipping effects pipeline and the real renderer at the default 260 ms, at
// 1200 ms and at 60 ms, every kept frame's trail light summed against its clean twin.

use crate::trail_letters_read::{
    DARK, Ev, GITHUB_LIGHT, Kept, Look, Replay, Spec, TYPING_TEXT, typed,
};
use aterm_effects::cursor_fx::TrailColor;
use aterm_effects::cursor_glow::{GlowStyle, KnobContext, TrailKnob};

/// Every trail style the Settings picker offers, and the rainbow's other bodies and pets
/// ([`Spec`]).
const STYLES: [&str; 14] = [
    "rainbow + cat",
    "underline rainbow + cat",
    "rainbow + dog",
    "flat rainbow + cat",
    "rainbow",
    "phaser",
    "fire",
    "water",
    "laser",
    "comet",
    "sparkle",
    "lumen",
    "classic",
    "beam",
];

/// A channel off its clean twin's by more than this is the effects' (`trail-contrast`'s
/// `diff_vs_clean`).
const DIFF_DELTA: u32 = 12;

/// The cells either side of the caret, and the row above and below them, whose light is the
/// head's (its crown, halo and bed), not the trail it leaves behind.
const HEAD_CELLS: usize = 2;

/// The typing scene at `duration_ms`: the prompt `$ ` on row 4 at 200 ms and the line typed at
/// 60 ms a key from 1 s, `style` on `look`, kept every 48 ms from the first key to 1.2 s after
/// the last (the longest trail's fade); `scaled` off is the negative control
/// (`EffectsPipeline::set_typed_life_scaled(false)`). The trail's light ([`trail_light_of`])
/// summed over the kept frames.
fn trail_light(style: &str, look: Look, duration_ms: u64, scaled: bool) -> u64 {
    let mut r = Replay::new(style, look, TrailColor::Automatic);
    let classic = style.eq_ignore_ascii_case("classic");
    let spec = Spec::of(style);
    r.fx.set_cursor_glow(
        true,
        spec.trail,
        TrailColor::Automatic,
        None,
        duration_ms,
        24,
        if classic { 0.7 } else { 1.0 },
        0.6,
        true,
        look.cursor,
    );
    r.fx.set_ribbon_shape(spec.tall, spec.flat);
    r.fx.set_typed_life_scaled(scaled);
    let last = 1_000 + 60 * (TYPING_TEXT.len() as u64 - 1);
    r.keep = (1_008..last + 1_200).step_by(48).collect();
    let mut ev = vec![(200, Ev::Out(b"\x1b[2J\x1b[5H$ ".to_vec()))];
    ev.extend(typed(TYPING_TEXT, 1_000, 60));
    let _ = r.run(&ev, 1_300);
    assert!(
        r.kept.len() >= 40,
        "fixture: {style} on {} keeps its frames ({})",
        look.name,
        r.kept.len()
    );
    r.kept.iter().map(trail_light_of).sum()
}

/// `k`'s trail light: the summed channel distance off its clean twin of every pixel some
/// channel of which moved more than [`DIFF_DELTA`], outside the head's box (the caret's cell
/// and [`HEAD_CELLS`] either side of it, a row above and below).
fn trail_light_of(k: &Kept) -> u64 {
    let (cw, ch) = k.cell;
    let (cx0, cy0) = (k.left + k.caret.1 * cw, k.top + k.caret.0 * ch);
    let (x0, x1) = (
        cx0.saturating_sub(HEAD_CELLS * cw),
        cx0 + (HEAD_CELLS + 1) * cw,
    );
    let (y0, y1) = (cy0.saturating_sub(ch), cy0 + 2 * ch);
    k.px.iter()
        .zip(&k.clean)
        .enumerate()
        .filter(|&(i, _)| {
            let (x, y) = (i % k.w, i / k.w);
            !((x0..x1).contains(&x) && (y0..y1).contains(&y))
        })
        .map(|(_, (&p, &c))| {
            let d = [16u32, 8, 0].map(|sh| ((p >> sh) & 0xff).abs_diff((c >> sh) & 0xff));
            if d.iter().any(|&v| v > DIFF_DELTA) {
                u64::from(d.iter().sum::<u32>())
            } else {
                0
            }
        })
        .sum()
}

/// TRAIL DURATION SHOWS IN EVERY STYLE THAT READS IT (access r3, r14 PV7: "a knob whose change
/// is near-invisible reads as broken"). A chained key's light lived as long as the key gap
/// needed to meet the next key's, whatever the duration, and its particles their own fixed
/// lives, so in Lumen, Phaser and Sparkle a person moving Trail duration from 260 ms to 1200 ms
/// or 60 ms saw nothing change, and Fire barely. Every typed light now lives its tuned life
/// times the duration's share of the default (`cursor_glow::typed_life_scale`), and the
/// default draws what it did. Typing the gallery's line on the dark page and on GitHub Light,
/// every style whose Settings row reads the knob (`GlowStyle::reads_knob`'s
/// `TrailKnob::Duration`: all but the Rainbow Kitty's, whose ribbon follows the typing and
/// whose row says so) leaves at least a fifth more trail light behind its head at 1200 ms than
/// at 260 ms, and at 60 ms at least a sixth less. Negative control
/// (`EffectsPipeline::set_typed_life_scaled(false)`, the lives before): one of Lumen's,
/// Phaser's and Sparkle's trails at 1200 ms, or Sparkle's and Comet's at 60 ms, on the dark
/// page, moves by under a fifth.
#[test]
fn trail_duration_shows_in_every_style_that_reads_it() {
    let reads: Vec<&str> = STYLES
        .into_iter()
        .filter(|s| {
            GlowStyle::parse(Spec::of(s).trail)
                .reads_knob(TrailKnob::Duration, KnobContext::widest(false))
        })
        .collect();
    assert_eq!(
        reads.len(),
        9,
        "the Rainbow Kitty's five spellings alone disclose Trail duration: {reads:?}"
    );
    let items: Vec<(&str, Look)> = reads
        .iter()
        .flat_map(|&s| [(s, DARK), (s, GITHUB_LIGHT)])
        .collect();
    let read: Vec<(&str, &str, [u64; 3])> = std::thread::scope(|scope| {
        let jobs: Vec<_> = items
            .iter()
            .map(|&(style, look)| {
                scope.spawn(move || {
                    let at = |ms| trail_light(style, look, ms, true);
                    (style, look.name, [at(60), at(260), at(1_200)])
                })
            })
            .collect();
        jobs.into_iter()
            .map(|j| j.join().expect("a replay"))
            .collect()
    });
    for &(style, page, [short, default, long]) in &read {
        assert!(
            default > 0,
            "fixture: {style} on {page} lays a trail at the default duration"
        );
        assert!(
            long * 5 >= default * 6,
            "{style} on {page}: 1200 ms leaves {long} of trail light against 260 ms's \
             {default}, under a fifth more"
        );
        assert!(
            short * 6 <= default * 5,
            "{style} on {page}: 60 ms leaves {short} of trail light against 260 ms's \
             {default}, under a sixth less"
        );
    }
    let old: Vec<(&str, u64, u64, u64)> = std::thread::scope(|scope| {
        let jobs: Vec<_> = [
            ("lumen", 1_200),
            ("phaser", 1_200),
            ("sparkle", 1_200),
            ("sparkle", 60),
            ("comet", 60),
        ]
        .into_iter()
        .map(|(style, ms)| {
            scope.spawn(move || {
                (
                    style,
                    ms,
                    trail_light(style, DARK, 260, false),
                    trail_light(style, DARK, ms, false),
                )
            })
        })
        .collect();
        jobs.into_iter()
            .map(|j| j.join().expect("a replay"))
            .collect()
    });
    assert!(
        old.iter()
            .any(|&(_, _, default, moved)| moved * 5 < default * 6 && moved * 6 > default * 5),
        "negative control: the old lives move every one of these trails by a fifth or more: \
         {old:?}"
    );
}
