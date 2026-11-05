// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// A trail colour the user chose is worn as itself: the trail gallery's typing scene replayed
// through the shipping effects pipeline and the real renderer, the host's post passes on, every
// kept frame read against its clean twin, as the gallery's pin captures (`pin-page`,
// `pin-pink`) were judged.

use crate::trail_letters_read::{
    DARK, DEFAULT_LIGHT, Ev, GITHUB_LIGHT, Kept, Look, Replay, SOLARIZED_DARK, Scene, TYPING_TEXT,
    typed,
};
use aterm_effects::cursor_fx::TrailColor;
use aterm_effects::pipeline::EffectsPipeline;

/// The styles whose trail reads the Color knob, the ones the gallery pinned.
const PINNED_STYLES: [&str; 5] = ["lumen", "comet", "beam", "laser", "sparkle"];

/// A near-neutral pick close to GitHub Light's page, chroma 8 (the gallery's `pin-page`).
const PAPER_PIN: u32 = 0x00E8_ECF0;
/// A pale lavender grey, chroma 24 (rev9fix's `sparkle-pin`).
const LAVENDER_PIN: u32 = 0x00B0_B0C8;
/// The gallery's pink pin (`pin-pink`, Classic pink's).
const PINK_PIN: u32 = 0x00D0_3A8C;
/// A pick close to the dark page (the gallery's `pin-page` on dark).
const SLATE_PIN: u32 = 0x0020_242C;
/// A light pink, the pink pin's bolt at its lightest: its bolt's clip falls short of its halo
/// ([`a_chosen_colour_s_bolt_caret_reads_against_its_own_halo`]).
const LIGHT_PINK_PIN: u32 = 0x00FF_76C1;

/// A channel off its clean twin's by more than this is the effects' (`trail-contrast`'s
/// `diff_vs_clean`).
const DIFF_DELTA: u32 = 12;

fn chroma(c: u32) -> u32 {
    let (r, g, b) = ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
    r.max(g).max(b) - r.min(g).min(b)
}

fn hue(c: u32) -> f32 {
    let (h, _, _) = aterm_effects::color_math::rgb2hsv(c & 0x00FF_FFFF);
    h
}

fn luminance(c: u32) -> f64 {
    let lin = |v: u32| {
        let v = f64::from(v) / 255.0;
        if v <= 0.040_45 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin((c >> 16) & 0xff) + 0.7152 * lin((c >> 8) & 0xff) + 0.0722 * lin(c & 0xff)
}

fn contrast(a: u32, b: u32) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// The typing scene: the prompt `$ ` on row 4 at 200 ms and the line typed at 60 ms a key from
/// 1 s, `style` in `pin` on `look`, the post passes on, with `controls` applied to the pipeline;
/// kept every 48 ms from the first key to 600 ms after the last.
fn typed_pinned(
    style: &str,
    look: Look,
    pin: u32,
    controls: impl Fn(&mut EffectsPipeline),
) -> Vec<Kept> {
    let mut r = Replay::new(style, look, TrailColor::Pinned(pin));
    r.post = true;
    controls(&mut r.fx);
    let end = 1_000 + 60 * TYPING_TEXT.len() as u64 + 600;
    r.keep = (1_008..end).step_by(48).collect();
    let mut ev = vec![(200, Ev::Out(b"\x1b[2J\x1b[5H$ ".to_vec()))];
    ev.extend(typed(TYPING_TEXT, 1_000, 60));
    let _ = r.run(&ev, 600);
    assert!(
        r.kept.len() >= 20,
        "fixture: {style} on {} keeps its frames ({})",
        look.name,
        r.kept.len()
    );
    std::mem::take(&mut r.kept)
}

/// [`typed_pinned`] of `style`, one of [`PINNED_STYLES`], in the pink pin on the dark page with no
/// control applied. Two tests read these frames, and a full run replays each style once.
fn pink_on_dark(style: &str) -> &'static [Kept] {
    static KEPT: [std::sync::OnceLock<Vec<Kept>>; PINNED_STYLES.len()] =
        [const { std::sync::OnceLock::new() }; PINNED_STYLES.len()];
    let at = PINNED_STYLES
        .iter()
        .position(|pinned| *pinned == style)
        .expect("a pinned style");
    KEPT[at].get_or_init(|| typed_pinned(style, DARK, PINK_PIN, |_| {}))
}

/// Every pixel of `k` the effects changed, outside the caret's cell: `(x, y, frame px)`.
fn changed(k: &Kept) -> impl Iterator<Item = (usize, usize, u32)> + '_ {
    let (cw, ch) = cell(k);
    let (cx0, cy0) = (k.left + k.caret.1 * cw, k.top + k.caret.0 * ch);
    k.px.iter()
        .zip(&k.clean)
        .enumerate()
        .filter_map(move |(i, (&f, &c))| {
            let (x, y) = (i % k.w, i / k.w);
            let caret = (cx0..cx0 + cw).contains(&x) && (cy0..cy0 + ch).contains(&y);
            let off = [16u32, 8, 0]
                .iter()
                .any(|&sh| ((f >> sh) & 0xff).abs_diff((c >> sh) & 0xff) > DIFF_DELTA);
            (off && !caret).then_some((x, y, f & 0x00FF_FFFF))
        })
}

/// The replay's cell size.
fn cell(k: &Kept) -> (usize, usize) {
    k.cell
}

/// The 95th percentile of `v` (0 when empty).
fn p95(mut v: Vec<u32>) -> u32 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[(v.len() * 95 / 100).min(v.len() - 1)]
}

/// A CHOSEN COLOUR LAYS ITS TRAIL IN ITS OWN CHROMA ON PAPER (composed r0's judges; the pin
/// ruling): a trail colour the user pinned is drawn as itself, so on a light page no part of
/// the trail wears more chroma than the pin. The paper deepening took a pin's hue to the most
/// saturation one ink weight allows, so a near-neutral `#e8ecf0` (chroma 8) inked Lumen's
/// tracer `#6da9e3` (chroma 117), Comet's tail and dust, Beam's tube and Laser's crackle in
/// blue on GitHub Light, where Sparkle drew it grey ([`LightGround::chosen`]). Every frame of
/// the typing scene, every pinned style, the near-neutral pin and a pale lavender: the pixels
/// the effects changed outside the caret's cell keep a 95th-percentile chroma within 6 of the
/// pin's. Negative control (`EffectsPipeline::set_chosen_ink(false)` alone, the near-neutral
/// pin): Lumen, Comet, Beam and Laser each lay their trail past twice that.
#[test]
fn a_chosen_colour_lays_its_trail_in_its_own_chroma_on_paper() {
    let worst = |style: &str, pin: u32, on: bool| -> (u32, u64) {
        typed_pinned(style, GITHUB_LIGHT, pin, |fx| fx.set_chosen_ink(on))
            .iter()
            .map(|k| (p95(changed(k).map(|(_, _, px)| chroma(px)).collect()), k.ms))
            .max()
            .unwrap_or((0, 0))
    };
    for pin in [PAPER_PIN, LAVENDER_PIN] {
        for style in PINNED_STYLES {
            let (p, at) = worst(style, pin, true);
            assert!(
                p <= chroma(pin) + 6,
                "{style} pinned to {pin:06x} (chroma {}) on GitHub Light: the trail's \
                 95th-percentile chroma reads {p} at {at} ms",
                chroma(pin)
            );
        }
    }
    for style in ["lumen", "comet", "beam", "laser"] {
        let (p, _) = worst(style, PAPER_PIN, false);
        assert!(
            p > 2 * (chroma(PAPER_PIN) + 6),
            "negative control: {style}'s paper trail under the pin reads chroma {p} without the law"
        );
    }
}

/// The share of `k`'s changed pixels of chroma 24 or more (the trail's colour, not a white
/// glint or a grey edge) whose hue lies within 25 degrees of `pin`'s, and how many there are.
/// A pixel with two channels at full is light the display clipped (a sparkle's core stacked
/// past white: `#ffc8ff`), whose hue is the clip's, not the light's, and is not counted.
fn share_in_hue(k: &Kept, pin: u32) -> (f64, usize) {
    let want = hue(pin);
    let clipped = |px: u32| {
        [16u32, 8, 0]
            .iter()
            .filter(|&&sh| (px >> sh) & 0xff == 0xff)
            .count()
            >= 2
    };
    let lit: Vec<f32> = changed(k)
        .filter(|&(_, _, px)| chroma(px) >= 24 && !clipped(px))
        .map(|(_, _, px)| hue(px))
        .collect();
    let near = lit
        .iter()
        .filter(|&&h| {
            let d = (h - want).abs();
            d.min(360.0 - d) <= 25.0
        })
        .count();
    (near as f64 / lit.len().max(1) as f64, lit.len())
}

/// COMET'S DUST WEARS A PINNED COLOUR (composed r0's judges; O1(b): no part of a pinned trail
/// keeps its built-in hue): under `#d03a8c` on the dark page the blade was pink and the dust
/// the tail's ice, hue 210, every chromatic pixel of `pin-pink/comet-dark.t120`. The dust is
/// the pin toward white, as the tail is. From the first key on (blade, coma, tail and dust),
/// every frame's coloured pixels lie within 25 degrees of the pin's hue, nine in ten. The hold
/// that wears every light of a pinned trail in the pin's hue on dark
/// (`EffectsPipeline::set_pin_chroma`) keeps this law by itself there, so both arms run with
/// the hold off and the law is judged alone. Negative control
/// (`EffectsPipeline::set_comet_pin_dust(false)`, the hold off in both arms): some frame's
/// coloured pixels lie off the pin's hue, under half of them near it.
#[test]
fn comet_s_dust_wears_a_pinned_colour() {
    let after = 1_000;
    let read = |on: bool| -> Vec<(u64, f64, usize)> {
        typed_pinned("comet", DARK, PINK_PIN, |fx| {
            fx.set_comet_pin_dust(on);
            fx.set_pin_chroma(false);
        })
        .iter()
        .filter(|k| k.ms >= after)
        .map(|k| {
            let (share, n) = share_in_hue(k, PINK_PIN);
            (k.ms, share, n)
        })
        .filter(|&(_, _, n)| n >= 8)
        .collect()
    };
    let frames = read(true);
    assert!(
        frames.len() >= 3,
        "fixture: the comet is lit in {} frames from the first key",
        frames.len()
    );
    for &(ms, share, n) in &frames {
        assert!(
            share >= 0.9,
            "Comet pinned to {PINK_PIN:06x} at {ms} ms: {share:.2} of its {n} coloured pixels \
             wear the pin's hue"
        );
    }
    let old = read(false);
    assert!(
        old.iter().any(|&(_, share, _)| share < 0.5),
        "negative control: the iced dust wears the pin's hue in every frame: {old:?}"
    );
}

/// COMET LAYS NO ICY HEAD UNDER A PINNED COLOUR (composed r1's gallery): the nucleus under the
/// caret and a dense row's seat past it ([`aterm_effects::cursor_glow`]'s `push_comet_seat`) are
/// the tail's ice, laid only for the unpinned comet; moved to Comet's emitter they were laid
/// under a pin too, and `pin-pink/comet-dark-context` drew an ice-blue head past a pink caret
/// (74% of its coloured trail pixels at hue 212). Typing a dense row (lettered rows above and
/// below it, the gallery's `context`) under `#d03a8c` on the dark page, every frame from the
/// first key lays its coloured pixels within 25 degrees of the pin's hue, nine in ten. The hold
/// that wears every light of a pinned trail in the pin's hue on dark
/// (`EffectsPipeline::set_pin_chroma`) keeps this law by itself there, so both arms run with
/// the hold off and the law is judged alone. Negative control
/// (`EffectsPipeline::set_comet_pin_head(false)`, the hold off in both arms): some frame lays
/// under half.
#[test]
fn comet_lays_no_icy_head_under_a_pinned_colour() {
    let read = |on: bool| -> Vec<(u64, f64, usize)> {
        let mut r = Replay::new("comet", DARK, TrailColor::Pinned(PINK_PIN));
        r.post = true;
        r.fx.set_comet_pin_head(on);
        r.fx.set_pin_chroma(false);
        let end = 1_000 + 60 * TYPING_TEXT.len() as u64 + 600;
        r.keep = (1_008..end).step_by(48).collect();
        let rule = "Above: every glyph here must stay legible, pqgyj {braces} [ok]";
        let below = "src/trail.rs:412  a lettered line under the prompt, all across";
        let page = format!("\x1b[2J\x1b[4H{rule}\x1b[6H{below}\x1b[5H$ ");
        let mut ev = vec![(200, Ev::Out(page.into_bytes()))];
        ev.extend(typed(TYPING_TEXT, 1_000, 60));
        let _ = r.run(&ev, 600);
        r.kept
            .iter()
            .map(|k| {
                let (share, n) = share_in_hue(k, PINK_PIN);
                (k.ms, share, n)
            })
            .filter(|&(_, _, n)| n >= 8)
            .collect()
    };
    let frames = read(true);
    assert!(
        frames.len() >= 3,
        "fixture: the comet is lit in {} frames from the first key",
        frames.len()
    );
    for &(ms, share, n) in &frames {
        assert!(
            share >= 0.9,
            "Comet pinned to {PINK_PIN:06x} in a dense row at {ms} ms: {share:.2} of its {n} \
             coloured pixels wear the pin's hue"
        );
    }
    let iced = read(false);
    assert!(
        iced.iter().any(|&(_, share, _)| share < 0.5),
        "negative control: the icy head wears the pin's hue in every frame: {iced:?}"
    );
}

/// The caret's reading as `trail-contrast` takes it: its ink, the cell's pixels within the
/// square root of the peak's ratio to the page, read against the closer of the page and the
/// median of the cell's other pixels (the halo it sits in) where those outnumber the ink.
fn caret_reading(k: &Kept, page: u32) -> f64 {
    let (cw, ch) = cell(k);
    let (x0, y0) = (k.left + k.caret.1 * cw, k.top + k.caret.0 * ch);
    let mut px = Vec::with_capacity(cw * ch);
    for y in y0..y0 + ch {
        for x in x0..x0 + cw {
            px.push(k.px[y * k.w + x] & 0x00FF_FFFF);
        }
    }
    let peak = px
        .iter()
        .map(|&p| contrast(p, page))
        .fold(f64::MIN, f64::max);
    let floor = peak.sqrt().max(1.1);
    let (ink, rest): (Vec<u32>, Vec<u32>) = px.iter().partition(|&&p| contrast(p, page) >= floor);
    let median = |v: &[u32]| -> Option<u32> {
        if v.is_empty() {
            return None;
        }
        let mut out = 0;
        for sh in [16u32, 8, 0] {
            let mut c: Vec<u32> = v.iter().map(|p| (p >> sh) & 0xff).collect();
            c.sort_unstable();
            out |= c[c.len() / 2] << sh;
        }
        Some(out)
    };
    let Some(colour) = median(&ink) else {
        return f64::INFINITY;
    };
    if ink.len() * 8 < px.len() {
        return f64::INFINITY;
    }
    let ground = if rest.len() > ink.len() {
        median(&rest).unwrap_or(page)
    } else {
        page
    };
    contrast(colour, page).min(contrast(colour, ground))
}

/// A CHOSEN COLOUR'S BOLT CARET READS AGAINST ITS OWN HALO (composed r0's judges; O1(a), which
/// does not extend Laser's bolt-caret ruling to pins): Laser's bolt in a pinned colour sits in
/// that colour's aura, and floored against the page alone it read 2.60:1 against its halo under
/// a slate pin on dark (`pin-page/laser-dark.t120`), 3.93 under the pink pin and 4.24 on paper.
/// It is lifted in lightness alone, its hue and chroma kept
/// (`aterm_effects::cursor_fx::chosen_bolt_over_halo`). Every frame of the typing scene, its
/// caret read as `trail-contrast` reads it (its ink against the closer of the page and the halo
/// round it) at 4.5:1 or more: slate, pink and a light pink on dark, the near-neutral pin on
/// GitHub Light. Where the lift meets a channel's end short of the bar it goes on toward white
/// (dark) or black (paper), keeping its hue: the pink pin's bolt read 4.41:1 at its clip with
/// every light of the trail worn in the pin (`CursorGlow::hold_pinned_lights`), and since the
/// letter guard reads each letter by the ring round its strokes (round 27) the light pink pin's
/// (the pink pin's bolt at its lightest, `#ff76c1`) reads 4.14 there. Negative controls
/// (`EffectsPipeline::set_chosen_bolt_lift(false)` alone, the slate pin): some frame's caret
/// reads under 4.5; (`EffectsPipeline::set_chosen_bolt_past_clip(false)` alone, the light pink
/// pin): some frame's caret reads under 4.5.
#[test]
fn a_chosen_colour_s_bolt_caret_reads_against_its_own_halo() {
    let worst_with = |look: Look, pin: u32, controls: &dyn Fn(&mut EffectsPipeline)| {
        typed_pinned("laser", look, pin, controls)
            .iter()
            .map(|k| (caret_reading(k, look.bg), k.ms))
            .fold((f64::INFINITY, 0), |a, b| if b.0 < a.0 { b } else { a })
    };
    let worst = |look: Look, pin: u32, on: bool| -> (f64, u64) {
        worst_with(look, pin, &|fx| fx.set_chosen_bolt_lift(on))
    };
    for (look, pin) in [
        (DARK, SLATE_PIN),
        (DARK, PINK_PIN),
        (DARK, LIGHT_PINK_PIN),
        (GITHUB_LIGHT, PAPER_PIN),
    ] {
        let (c, at) = worst(look, pin, true);
        assert!(
            c >= 4.5,
            "Laser pinned to {pin:06x} on {}: its bolt caret reads {c:.2}:1 at {at} ms",
            look.name
        );
    }
    let (c, _) = worst(DARK, SLATE_PIN, false);
    assert!(
        c < 4.5,
        "negative control: unlifted, the slate bolt still reads {c:.2}:1"
    );
    let (c, _) = worst_with(DARK, LIGHT_PINK_PIN, &|fx| {
        fx.set_chosen_bolt_past_clip(false)
    });
    assert!(
        c < 4.5,
        "negative control: stopped at its clip, the light pink bolt still reads {c:.2}:1"
    );
}

/// A CHOSEN COLOUR LAYS ITS LIGHT IN ITS OWN HUE AND CHROMA ON A DARK PAGE (composed r1's
/// judges, O2(b), O4(d); the pin ruling: a colour the user chose keeps its hue and chroma). On
/// the dark page each style lifted a pin to the light it lays by scaling its channels, which
/// keeps its HSV saturation and gains chroma with its value: the slate `#20242c` (chroma 12)
/// drew Comet's blade in `#99add5` (chroma 60), Sparkle's rail tip in `#bed1fa` and Beam's
/// sparks in their own violet and ice (`#b0a6f5`). Each light the frame lays is now worn in the
/// pin's hue at its own luminance, at no more chroma than the pin's light has there
/// (`CursorGlow::hold_pinned_lights`). Every frame of the typing scene, every pinned style: under
/// the slate pin and a pale lavender `#b0b0c8` (chroma 24) the pixels the effects changed outside
/// the caret's cell keep a 95th-percentile chroma within 6 of the pin's; under the pink pin
/// every frame's coloured pixels lie within 25 degrees of the pin's hue, nine in ten. Negative
/// control (`EffectsPipeline::set_pin_chroma(false)` alone, the slate pin): Comet, Beam and
/// Sparkle each lay their light past twice that chroma.
#[test]
fn a_chosen_colour_lays_its_light_in_its_own_hue_and_chroma_on_dark() {
    let worst = |style: &str, pin: u32, on: bool| -> (u32, u64) {
        typed_pinned(style, DARK, pin, |fx| fx.set_pin_chroma(on))
            .iter()
            .map(|k| (p95(changed(k).map(|(_, _, px)| chroma(px)).collect()), k.ms))
            .max()
            .unwrap_or((0, 0))
    };
    let pinned: Vec<(u32, &str)> = [SLATE_PIN, LAVENDER_PIN]
        .into_iter()
        .flat_map(|pin| PINNED_STYLES.map(|style| (pin, style)))
        .collect();
    aterm_spec::check::each(&pinned, |&(pin, style)| {
        let (p, at) = worst(style, pin, true);
        assert!(
            p <= chroma(pin) + 6,
            "{style} pinned to {pin:06x} (chroma {}) on the dark page: the trail's \
             95th-percentile chroma reads {p} at {at} ms",
            chroma(pin)
        );
    });
    aterm_spec::check::each(&PINNED_STYLES, |&style| {
        for k in pink_on_dark(style) {
            let (share, n) = share_in_hue(k, PINK_PIN);
            assert!(
                n < 8 || share >= 0.9,
                "{style} pinned to {PINK_PIN:06x} at {} ms: {share:.2} of its {n} coloured \
                 pixels wear the pin's hue",
                k.ms
            );
        }
    });
    aterm_spec::check::each(&["comet", "beam", "sparkle"], |&style| {
        let (p, _) = worst(style, SLATE_PIN, false);
        assert!(
            p > 2 * (chroma(SLATE_PIN) + 6),
            "negative control: {style}'s light under the slate pin reads chroma {p} without the \
             law"
        );
    });
}

/// A VIVID PIN'S TRAIL IS NO DULLER FOR THE HOLD (composed r2's identity judge, O9(e): "hue
/// and chroma stay" means neither raised nor cut). The hold that wears every light of a pinned
/// trail in the pin's hue on a dark page cut each light to the pin's share at its luminance,
/// and under the pink pin `#d03a8c` every pinned style's light read 20-27% duller than it laid
/// it (Classic pink's rail `#8d0954` to `#6e3357`). A light now keeps its own chroma up to the
/// pin's once it is bright enough not to stack past it (`CursorGlow::hold_pinned_lights`).
/// The typing scene on the dark page under the pink pin: every pinned style's changed pixels
/// keep a 95th-percentile chroma within a tenth of what they read without the hold, and no
/// frame's reads past the pin's chroma by more than 6, Classic's included. Classic is held to
/// the bound alone: its own lights lay past the pin's (184 at worst without the hold), so nine
/// tenths of its unheld reading is past the bound; its rail's chroma is judged on its own by
/// [`classic_pink_s_rail_wears_the_pin_s_chroma`]. Negative control (`EffectsPipeline::set_pin_hold_two_sided(false)`
/// alone): some style reads under nine tenths.
#[test]
fn a_vivid_pin_s_trail_is_no_duller_for_the_hold() {
    let judge = |kept: &[Kept]| -> (u32, u32, u64) {
        let all = kept
            .iter()
            .flat_map(|k| changed(k).map(|(_, _, px)| chroma(px)).collect::<Vec<_>>())
            .collect();
        let (worst, at) = kept
            .iter()
            .map(|k| (p95(changed(k).map(|(_, _, px)| chroma(px)).collect()), k.ms))
            .max()
            .unwrap_or((0, 0));
        (p95(all), worst, at)
    };
    let read = |style: &str, controls: &dyn Fn(&mut EffectsPipeline)| {
        judge(&typed_pinned(style, DARK, PINK_PIN, controls))
    };
    let styles: Vec<&str> = PINNED_STYLES.into_iter().chain(["classic"]).collect();
    let duller: Vec<&str> = aterm_spec::check::each(&styles, |&style| {
        let (held, worst, at) = if style == "classic" {
            read(style, &|_| {})
        } else {
            judge(pink_on_dark(style))
        };
        assert!(
            worst <= chroma(PINK_PIN) + 6,
            "{style} pinned to {PINK_PIN:06x}: a frame's 95th-percentile chroma reads {worst} at \
             {at} ms, past the pin's {}",
            chroma(PINK_PIN)
        );
        if style == "classic" {
            return None;
        }
        let (free, ..) = read(style, &|fx| fx.set_pin_chroma(false));
        assert!(
            10 * held >= 9 * free,
            "{style} pinned to {PINK_PIN:06x}: its light reads chroma {held} under the hold, \
             {free} without it"
        );
        let (one_sided, ..) = read(style, &|fx| fx.set_pin_hold_two_sided(false));
        (10 * one_sided < 9 * free).then_some(style)
    })
    .into_iter()
    .flatten()
    .collect();
    assert!(
        !duller.is_empty(),
        "negative control: one-sided, every pinned style still reads within a tenth"
    );
}

/// The hue gap in degrees between `a` and `b`.
fn hue_gap(a: f32, b: f32) -> f32 {
    let d = (a - b).abs();
    d.min(360.0 - d)
}

/// The light `k` lays on its page: every pixel the effects changed outside the caret's cell
/// whose clean twin is the page `bg` and which they brightened, of chroma 24 or more and not
/// clipped on two channels ([`share_in_hue`]), as `(luminance, hue)`.
fn lit_on_page(k: &Kept, bg: u32) -> Vec<(f64, f32)> {
    changed(k)
        .filter(|&(x, y, px)| {
            let clean = k.clean[y * k.w + x] & 0x00FF_FFFF;
            let clipped = [16u32, 8, 0]
                .iter()
                .filter(|&&sh| (px >> sh) & 0xff == 0xff)
                .count()
                >= 2;
            clean == bg && chroma(px) >= 24 && !clipped && luminance(px) > luminance(clean)
        })
        .map(|(_, _, px)| (luminance(px), hue(px)))
        .collect()
}

/// A pixel the trail gallery's judges read as ground (`stain.py`): changed from its clean twin
/// by more than 25 on some channel, at the luminance of what lay there within 0.6 to 1.6 times
/// (a light that lifts the page further is a light, not a ground).
fn ground_px(k: &Kept) -> impl Iterator<Item = (u32, u32)> + '_ {
    k.px.iter().zip(&k.clean).filter_map(|(&f, &c)| {
        let moved = [16u32, 8, 0]
            .iter()
            .any(|&sh| ((f >> sh) & 0xff).abs_diff((c >> sh) & 0xff) > 25);
        let (lf, lc) = (luminance(f), luminance(c));
        (moved && lf < lc * 1.6 + 0.004 && lf > lc * 0.6)
            .then_some((f & 0x00FF_FFFF, c & 0x00FF_FFFF))
    })
}

/// How Classic pinned pink reads on Solarized Dark under `controls`, every 24 ms from the
/// prompt's jump on through the gallery's typing, Enter at the bottom and context scenes and
/// a jump across blank rows: the least share of a frame's core (its lit pixels on the page
/// that read 2:1 or more against it, [`lit_on_page`]) within 25 degrees of the pin's hue, and
/// the most ground pixels a frame lays on the page ([`ground_px`]) off neutral (chroma past
/// 18, the pin laws' 6 to spare past 12) in a hue more than 60 degrees from the page's: a patch
/// the page's own glow is not, with the time of each.
fn classic_pink_on_solarized(controls: impl Fn(&mut EffectsPipeline)) -> (f64, u64, usize, u64) {
    let want = hue(PINK_PIN);
    let bg = SOLARIZED_DARK.bg;
    let run = |ev: Vec<(u64, Ev)>, tail: u64, from: u64| -> Vec<Kept> {
        let mut r = Replay::new("classic", SOLARIZED_DARK, TrailColor::Pinned(PINK_PIN));
        r.post = true;
        controls(&mut r.fx);
        let end = ev.last().map_or(0, |(at, _)| *at) + tail;
        r.keep = (from..end).step_by(24).collect();
        let _ = r.run(&ev, tail);
        std::mem::take(&mut r.kept)
    };
    let (mut core, mut core_at, mut patch, mut patch_at) = (1.0f64, 0u64, 0usize, 0u64);
    let mut judge = |kept: &[Kept]| {
        for k in kept {
            let page = luminance(bg);
            let lit: Vec<f32> = lit_on_page(k, bg)
                .into_iter()
                .filter(|l| (l.0 + 0.05) / (page + 0.05) >= 2.0)
                .map(|l| l.1)
                .collect();
            if lit.len() >= 8 {
                let near = lit.iter().filter(|&&h| hue_gap(h, want) <= 25.0).count();
                let share = near as f64 / lit.len() as f64;
                if share < core {
                    (core, core_at) = (share, k.ms);
                }
            }
            let off = ground_px(k)
                .filter(|&(f, c)| c == bg && chroma(f) > 18 && hue_gap(hue(f), hue(bg)) > 60.0)
                .count();
            if off > patch {
                (patch, patch_at) = (off, k.ms);
            }
        }
    };
    for scene in [Scene::Typing, Scene::EnterBottom, Scene::Context] {
        let (ev, tail) = scene.events_on(SOLARIZED_DARK);
        judge(&run(ev, tail, 208));
    }
    let mut ev = vec![(200, Ev::Out(b"\x1b[2J\x1b[5H$ ".to_vec()))];
    ev.extend(typed("echo", 1_000, 60));
    ev.push((1_600, Ev::Out(b"\x1b[12;40H".to_vec())));
    judge(&run(ev, 400, 1_600));
    (core, core_at, patch, patch_at)
}

/// A CHOSEN COLOUR KEEPS ITS HUE ON A TINTED DARK PAGE, ON THE PAGE'S OWN GROUND (composed r1's
/// beauty judge, O4(b), and composed r2's judges, O9(a); the pin ruling). Classic's one-colour
/// face adds its light to the page, and Solarized Dark's teal `#002b36` summed into the pink pin
/// `#d03a8c` (hue 327) drew its jump core lilac (`#eeb0f6`, hue 293) and its Enter streak
/// blue-teal (219). Composed r2 laid a night ground solved toward the light's own hue, which
/// drew a maroon disc round the caret on the teal page (`#40212e`, 1356 px in the gallery's
/// `classic-dark.t0`), a stain rather than a glow. Its streak, its caret glow and its ring now
/// lay the page's neutral under them, as Fire's and Sparkle's lights do
/// (`aterm_effects::cursor_glow::GroundRule::NeutralCore`), its faint skirt fading into the teal
/// as a glow does. The gallery's typing, Enter at the bottom and context scenes and a jump
/// across blank rows, the post passes on, every 24 ms: nine in ten of every frame's core pixels
/// lie within 25 degrees of the pin's hue, and no frame lays more than 100 pixels at the
/// page's luminance off neutral in a hue away from the page's: the dark edges of its rail and
/// streak (40 to 58 a frame), never a disc (composed r2's ground laid 700 to 5800, as many as
/// the light round it). Negative controls:
/// `EffectsPipeline::set_classic_pin_ground(false)` (the light added to the teal) puts some
/// frame's core under half; `set_classic_ground_neutral(false)` (composed r2's ground) lays
/// hundreds.
#[test]
fn a_chosen_colour_keeps_its_hue_on_a_tinted_dark_page() {
    let (core, core_at, patch, patch_at) = classic_pink_on_solarized(|_| {});
    assert!(
        core >= 0.9,
        "Classic pinned to {PINK_PIN:06x} on Solarized Dark: {core:.2} of the core at {core_at} \
         ms wears the pin's hue"
    );
    assert!(
        patch <= 100,
        "Classic pinned to {PINK_PIN:06x} on Solarized Dark: {patch} ground pixels at {patch_at} \
         ms lie off neutral, away from the page's hue"
    );
    let (core, ..) = classic_pink_on_solarized(|fx| fx.set_classic_pin_ground(false));
    assert!(
        core < 0.5,
        "negative control: added to the teal, {core:.2} of a frame's core still wears the pin"
    );
    let (.., patch, _) = classic_pink_on_solarized(|fx| fx.set_classic_ground_neutral(false));
    assert!(
        patch > 400,
        "negative control: on composed r2's ground only {patch} ground pixels lie off neutral"
    );
}

/// The gallery's `steam` scene: the typing scene's line, then twelve Backspaces 50 ms apart
/// from 300 ms after its last key, `style` in the theme's own colour on `look`, the post passes
/// on, `controls` applied; kept every 16 ms from the first key to a second after the first
/// Backspace.
fn steamed(style: &str, look: Look, controls: impl Fn(&mut EffectsPipeline)) -> Vec<Kept> {
    let mut r = Replay::new(style, look, TrailColor::Automatic);
    r.post = true;
    controls(&mut r.fx);
    let mut ev = vec![(200, Ev::Out(b"\x1b[2J\x1b[5H$ ".to_vec()))];
    ev.extend(typed(TYPING_TEXT, 1_000, 60));
    let start = 1_000 + 60 * TYPING_TEXT.len() as u64 + 300;
    for k in 0..12u64 {
        ev.push((start + 50 * k, Ev::Key('\u{7f}', b"\x08\x1b[K".to_vec())));
    }
    r.keep = (1_008..start + 1_000).step_by(16).collect();
    let _ = r.run(&ev, 1_000);
    std::mem::take(&mut r.kept)
}

/// LASER'S PAPER BOLT READS AGAINST ITS OWN HALO (composed r1's access judge, O5(a): no ruling
/// excuses a caret under 4.5 on paper). In the theme's own colour Laser's bolt `#7834f9` sat in
/// the lavender halo the steam's Backspaces feed (`#cab9ec`) at 3.72:1 on GitHub Light and 3.85
/// on Default Light (the trail gallery's `laser-light-steam.bs09` read 4.41 and 4.40); a chosen
/// colour's bolt was lifted against its halo, the theme's was not, and the lift read the over
/// stream alone, where a paper halo lies under the letters. It is lifted in lightness alone as
/// a chosen colour's is, against the frame's under and over streams and halos
/// (`aterm_effects::cursor_fx::chosen_bolt_over_halo`). Every frame of the steam scene, its
/// caret read as `trail-contrast` reads it, at 4.5:1 or more on both light themes. Negative
/// control (`EffectsPipeline::set_paper_bolt_lift(false)` alone): some frame reads under 4.5.
#[test]
fn laser_s_paper_bolt_reads_against_its_own_halo() {
    let worst = |look: Look, on: bool| -> (f64, u64) {
        steamed("laser", look, |fx| fx.set_paper_bolt_lift(on))
            .iter()
            .map(|k| (caret_reading(k, look.bg), k.ms))
            .fold((f64::INFINITY, 0), |a, b| if b.0 < a.0 { b } else { a })
    };
    for look in [GITHUB_LIGHT, DEFAULT_LIGHT] {
        let (c, at) = worst(look, true);
        assert!(
            c >= 4.5,
            "Laser on {}: its bolt caret reads {c:.2}:1 at {at} ms",
            look.name
        );
    }
    let (c, _) = worst(GITHUB_LIGHT, false);
    assert!(
        c < 4.5,
        "negative control: unlifted, the paper bolt still reads {c:.2}:1"
    );
}

/// `c`'s OKLCH chroma per unit of its lightness ([`aterm_types::scheme_signature::oklab`]): a
/// light dimmed or brightened in its own channels keeps it, so a rail's pixel lit at part
/// strength reads it as its colour would at full.
fn chroma_per_lightness(c: u32) -> f64 {
    let (l, a, b) = aterm_types::scheme_signature::oklab(aterm_types::Rgb {
        r: (c >> 16) as u8,
        g: (c >> 8) as u8,
        b: c as u8,
    });
    a.hypot(b) / l.max(1e-6)
}

/// The rail's core in `k`: the pixels the effects lit on the page in the band under the caret's
/// row (the one-colour face's rail where the row below is air), left of the caret, at 2:1 or
/// more against the page.
fn rail_core(k: &Kept, page: u32) -> Vec<u32> {
    let (cw, ch) = cell(k);
    let y_row_end = k.top + (k.caret.0 + 1) * ch;
    let x_caret = k.left + k.caret.1 * cw;
    let mut out = Vec::new();
    for y in y_row_end.saturating_sub(ch / 3)..y_row_end + ch / 2 {
        for x in k.left..x_caret {
            let (p, c) = (
                k.px[y * k.w + x] & 0x00FF_FFFF,
                k.clean[y * k.w + x] & 0x00FF_FFFF,
            );
            if c == page && contrast(p, page) >= 2.0 {
                out.push(p);
            }
        }
    }
    out
}

/// CLASSIC PINK'S RAIL WEARS THE PIN'S CHROMA (composed O10(a): "hue and chroma stay", neither
/// raised nor cut). Classic's one-colour face floored a chosen colour's bed to a saturation of
/// 0.9 and its rail to 1.0, laying the pink pin `#d03a8c` (chroma 150) at 196, and its rail read
/// up to 18% more vivid than the pin where the pinned lights' hold left it whole; laid at the
/// pin's own saturation instead, the dark page's grey dilutes it 9-16% duller. The rail now
/// composes on the page to the pin ([`aterm_effects::classic_wake`]'s `pinned_rail_ink`).
/// Typing the gallery's line on the dark page under the pink pin, every frame's rail core (its
/// pixels at 2:1 or more on the page under the caret's row) reads a median OKLCH chroma per
/// lightness within a tenth of the pin's. Negative control
/// (`EffectsPipeline::set_classic_pin_own_chroma(false)`, the old saturation floors): some
/// frame reads past a tenth.
#[test]
fn classic_pink_s_rail_wears_the_pin_s_chroma() {
    let want = chroma_per_lightness(PINK_PIN);
    let read = |own: bool| -> Vec<(u64, f64)> {
        typed_pinned("classic", DARK, PINK_PIN, |fx| {
            fx.set_classic_pin_own_chroma(own);
        })
        .iter()
        .filter_map(|k| {
            let mut c: Vec<f64> = rail_core(k, DARK.bg)
                .into_iter()
                .map(chroma_per_lightness)
                .collect();
            (c.len() >= 8).then(|| {
                c.sort_by(f64::total_cmp);
                (k.ms, c[c.len() / 2])
            })
        })
        .collect()
    };
    let frames = read(true);
    assert!(
        frames.len() >= 20,
        "fixture: Classic pink lays its rail in {} frames",
        frames.len()
    );
    for &(ms, c) in &frames {
        assert!(
            (c / want - 1.0).abs() <= 0.10,
            "Classic pinned to {PINK_PIN:06x} at {ms} ms: its rail reads chroma {c:.3} per \
             lightness against the pin's {want:.3}"
        );
    }
    let floored = read(false);
    assert!(
        floored.iter().any(|&(_, c)| (c / want - 1.0).abs() > 0.10),
        "negative control: the old floors keep the rail within a tenth of the pin: {floored:?}"
    );
}
