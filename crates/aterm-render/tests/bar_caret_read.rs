// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A bar caret reads against the light of its cell: the trail gallery's bar scenes replayed
//! through the shipping effects pipeline and the real renderer, the host's post passes on, the
//! bar read as `trail-contrast`'s thin-caret reader reads it.

use crate::trail_letters_read::{DARK, Ev, GITHUB_LIGHT, Kept, Look, Replay, STYLES, Scene};
use aterm_effects::cursor_fx::TrailColor;
use aterm_effects::pipeline::EffectsPipeline;

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

/// The per-channel median of `v` (`None` when empty), as `trail-contrast` takes it.
fn median(v: &[u32]) -> Option<u32> {
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
}

/// A column of a pixel run's median contrast to the page past which it is drawn
/// (`trail-contrast`'s `CARET_DIFF`).
const CARET_DIFF: f64 = 1.1;

/// The bar's reading as `trail-contrast`'s thin-caret reader takes it: its span the columns the
/// clean twin draws it in (the strongest of the cell's left quarter and the run about it of
/// columns whose median reads [`CARET_DIFF`] against the page), its ink the median of the
/// frame's pixels there, read against the closer of the page and the median of the cell's
/// other pixels. `None` where the clean twin draws no bar (a blink's off phase).
pub fn bar_reading(k: &Kept, page: u32) -> Option<(f64, u32, u32)> {
    let (cw, ch) = k.cell;
    let (x0, y0) = (k.left + k.caret.1 * cw, k.top + k.caret.0 * ch);
    let col = |px: &[u32], x: usize| -> Vec<u32> {
        (y0..y0 + ch)
            .map(|y| px[y * k.w + x0 + x] & 0x00FF_FFFF)
            .collect()
    };
    let strength = |x: usize| {
        let mut c: Vec<f64> = col(&k.clean, x)
            .iter()
            .map(|&p| contrast(p, page))
            .collect();
        c.sort_unstable_by(f64::total_cmp);
        c[c.len() / 2]
    };
    let quarter = cw.div_ceil(4).max(1);
    let seat = (0..quarter).max_by(|&a, &b| strength(a).total_cmp(&strength(b)))?;
    if strength(seat) < CARET_DIFF {
        return None;
    }
    let (mut lo, mut hi) = (seat, seat + 1);
    while lo > 0 && strength(lo - 1) >= CARET_DIFF {
        lo -= 1;
    }
    while hi < cw && strength(hi) >= CARET_DIFF {
        hi += 1;
    }
    let (mut ink, mut rest) = (Vec::new(), Vec::new());
    for x in 0..cw {
        let c = col(&k.px, x);
        if (lo..hi).contains(&x) {
            ink.extend(c);
        } else {
            rest.extend(c);
        }
    }
    let colour = median(&ink)?;
    let ground = median(&rest).unwrap_or(page);
    Some((
        contrast(colour, page).min(contrast(colour, ground)),
        colour,
        ground,
    ))
}

/// `scene` with a steady bar caret (`\e[6 q` before its first output), `style` on `look`, the
/// host's post passes on (the bloom and the SDR crown), `controls` applied; every frame kept
/// from the first key on.
pub fn bar_scene(
    style: &str,
    look: Look,
    scene: Scene,
    controls: impl Fn(&mut EffectsPipeline),
) -> Vec<Kept> {
    let mut r = Replay::new(style, look, TrailColor::Automatic);
    r.post = true;
    controls(&mut r.fx);
    let (mut ev, tail) = scene.events_on(look);
    if let Some((_, Ev::Out(first))) = ev.first_mut() {
        let mut shaped = b"\x1b[6 q".to_vec();
        shaped.extend_from_slice(first);
        *first = shaped;
    }
    let end = ev.last().map_or(0, |(at, _)| *at) + tail;
    r.keep = (1_008..end).step_by(24).collect();
    let _ = r.run(&ev, tail);
    std::mem::take(&mut r.kept)
}

/// The caret's legibility ruling: it reads 4.5:1 or more against its cell.
const CARET_MIN: f64 = 4.5;

/// The scenes a bar caret is read in: the gallery's `shape-bar` set.
const SCENES: [Scene; 3] = [Scene::Typing, Scene::EnterBottom, Scene::Context];

/// Every bar reading of `style` on `look` across [`SCENES`], `(reading, ms, ink, ground,
/// the ground the effects read)`, `controls` applied.
fn readings(
    style: &str,
    look: Look,
    controls: impl Fn(&mut EffectsPipeline) + Copy,
) -> Vec<(f64, u64, u32, u32, Option<u32>)> {
    SCENES
        .iter()
        .flat_map(|&scene| {
            bar_scene(style, look, scene, controls)
                .iter()
                .filter_map(|k| {
                    bar_reading(k, look.bg)
                        .map(|(c, ink, ground)| (c, k.ms, ink, ground, k.caret_ground))
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Whether two colours lie within `d` levels on every channel.
fn near(a: u32, b: u32, d: u32) -> bool {
    [16, 8, 0]
        .iter()
        .all(|sh| ((a >> sh) & 0xff).abs_diff((b >> sh) & 0xff) <= d)
}

/// A THIN CARET READS AGAINST ITS CELL (composed r2, O2(a);
/// `aterm-effects/src/cursor_glow/ink_ledger/letter_guard/thin_caret.rs`): on `look`, every
/// trail style's bar reads 4.5:1 or more against the page and the light of its cell in the
/// gallery's bar scenes, the host's post passes on, read as `trail-contrast`'s thin-caret
/// reader reads it; and the ground the effects read it against is the frame's, the bloom and
/// the crown kept off the caret's cell (`aterm_render::bloom::thin_caret_cell`). Classic's
/// spectrum face on a dark theme keeps its bytes and its bloom (the v0.28 ruling, a ruled
/// exception) and is not read there. NEGATIVE CONTROL (`set_thin_caret_read(false)`): the bar
/// drawn in its own colour on the light the trail lays there reads under the ruling.
fn bars_read_on(look: Look, control: (&[&str], Scene)) {
    let styles: Vec<&str> = STYLES
        .iter()
        .copied()
        .chain(["off"])
        .filter(|style| look.bg != DARK.bg || *style != "classic")
        .collect();
    let under: Vec<String> = aterm_spec::check::each(&styles, |&style| {
        let read = readings(style, look, |fx| fx.set_thin_caret_read(true));
        assert!(
            !read.is_empty(),
            "{style} on {}: no bar was read",
            look.name
        );
        let mut under = Vec::new();
        for &(c, ms, ink, ground, effects) in &read {
            if c < CARET_MIN {
                under.push(format!(
                    "{style} @{ms} {c:.2} ink {ink:06x} on {ground:06x}"
                ));
            }
            if let Some(seen) = effects {
                assert!(
                    near(seen, ground, 2),
                    "{style} on {} @{ms}: the effects read the bar's ground {seen:06x}, the frame lays {ground:06x}",
                    look.name
                );
            }
        }
        under
    })
    .into_iter()
    .flatten()
    .collect();
    assert!(
        under.is_empty(),
        "bars under {CARET_MIN} on {}: {under:?}",
        look.name
    );
    let (styles, scene) = control;
    let worst = styles
        .iter()
        .flat_map(|style| {
            bar_scene(style, look, scene, |fx| fx.set_thin_caret_read(false))
                .iter()
                .filter_map(|k| bar_reading(k, look.bg).map(|(c, ..)| c))
                .collect::<Vec<_>>()
        })
        .fold(f64::INFINITY, f64::min);
    assert!(
        worst < CARET_MIN,
        "control: a bar in its own colour reads {worst:.2} at worst on {}",
        look.name
    );
}

#[test]
fn a_bar_caret_reads_against_its_cell_on_the_dark_page() {
    bars_read_on(DARK, (&["laser", "comet"], Scene::EnterBottom));
}

#[test]
fn a_bar_caret_reads_against_its_cell_on_paper() {
    bars_read_on(GITHUB_LIGHT, (&["flat rainbow", "comet"], Scene::Typing));
}
