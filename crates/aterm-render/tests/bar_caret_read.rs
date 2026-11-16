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
    bars_read_on(
        GITHUB_LIGHT,
        (&["flat rainbow + cat", "comet"], Scene::Typing),
    );
}

/// The columns the clean twin draws the bar in ([`bar_reading`]'s span), from the cell's left.
fn bar_span(k: &Kept, page: u32) -> Option<(usize, usize)> {
    let (cw, ch) = k.cell;
    let (x0, y0) = (k.left + k.caret.1 * cw, k.top + k.caret.0 * ch);
    let strength = |x: usize| {
        let mut c: Vec<f64> = (y0..y0 + ch)
            .map(|y| contrast(k.clean[y * k.w + x0 + x] & 0x00FF_FFFF, page))
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
    Some((lo, hi))
}

/// The bar read as a reader sees it, as `trail-contrast`'s thin-caret reader takes its ring:
/// each pixel of its line against the median of the frame's pixels on its nearest Chebyshev
/// ring (1, else 2) that are the cell's ground in the clean twin (its commonest colour off the
/// line), the worst pixel row's lower median. `None` where the clean twin draws no bar.
pub fn bar_ring_reading(k: &Kept, page: u32) -> Option<f64> {
    let (cw, ch) = k.cell;
    let (lo, hi) = bar_span(k, page)?;
    let (x0, y0) = (k.left + k.caret.1 * cw, k.top + k.caret.0 * ch);
    let h = k.px.len() / k.w;
    let mut counts: std::collections::HashMap<u32, (usize, usize)> = Default::default();
    let mut i = 0;
    for y in y0..y0 + ch {
        for x in 0..cw {
            if !(lo..hi).contains(&x) {
                counts
                    .entry(k.clean[y * k.w + x0 + x] & 0x00FF_FFFF)
                    .or_insert((0, i))
                    .0 += 1;
                i += 1;
            }
        }
    }
    let ground = counts
        .iter()
        .max_by(|a, b| a.1.0.cmp(&b.1.0).then(b.1.1.cmp(&a.1.1)))
        .map(|(&c, _)| c)?;
    let mut worst = f64::INFINITY;
    for y in y0..y0 + ch {
        let mut row = Vec::new();
        for x in x0 + lo..x0 + hi {
            let mut ring = Vec::new();
            for d in 1..=2i64 {
                for dy in -d..=d {
                    for dx in -d..=d {
                        if dx.abs().max(dy.abs()) != d {
                            continue;
                        }
                        let (xx, yy) = (x as i64 + dx, y as i64 + dy);
                        if xx < 0 || yy < 0 || xx as usize >= k.w || yy as usize >= h {
                            continue;
                        }
                        let j = yy as usize * k.w + xx as usize;
                        if k.clean[j] & 0x00FF_FFFF == ground {
                            ring.push(k.px[j] & 0x00FF_FFFF);
                        }
                    }
                }
                if !ring.is_empty() {
                    break;
                }
            }
            if let Some(g) = median(&ring) {
                row.push(contrast(k.px[y * k.w + x] & 0x00FF_FFFF, g));
            }
        }
        if !row.is_empty() {
            row.sort_unstable_by(f64::total_cmp);
            worst = worst.min(row[(row.len() - 1) / 2]);
        }
    }
    worst.is_finite().then_some(worst)
}

/// The scenes a bar caret's ring is read in: a band head beside the caret (typing, a wrapped
/// line, a coloured focus row) and a letter under it (move-up).
const RING_SCENES: [Scene; 4] = [
    Scene::Typing,
    Scene::Wrapped,
    Scene::FocusWrap,
    Scene::MoveUp,
];

/// A THIN CARET READS AGAINST ITS RING (CT-51 Rule A; `thin_caret.rs`): Rainbow's bar reads
/// 4.5:1 or more against the light beside its line in every frame of [`RING_SCENES`], its
/// cell's ground read with the letter under it as the frame lays it (the effects' ground within
/// two levels of the frame's; `thin_caret_tests` holds the letter's part with its control).
/// NEGATIVE CONTROL: read against its cell alone (`set_thin_caret_ring(false)`), the bar reads
/// under the ruling against its ring on some frame.
fn rings_read_on(look: Look) {
    let read = |controls: fn(&mut EffectsPipeline)| -> Vec<(Scene, u64, f64, bool)> {
        aterm_spec::check::each(&RING_SCENES, |&scene| {
            bar_scene("rainbow", look, scene, controls)
                .iter()
                .filter_map(|k| {
                    let ring = bar_ring_reading(k, look.bg)?;
                    let agrees = match (bar_reading(k, look.bg), k.caret_ground) {
                        (Some((_, _, ground)), Some(seen)) => near(seen, ground, 2),
                        _ => true,
                    };
                    Some((scene, k.ms, ring, agrees))
                })
                .collect::<Vec<_>>()
        })
        .into_iter()
        .flatten()
        .collect()
    };
    let shipped = read(|_| {});
    assert!(!shipped.is_empty(), "no bar read on {}", look.name);
    let under: Vec<String> = shipped
        .iter()
        .filter(|r| r.2 < CARET_MIN || !r.3)
        .map(|(s, ms, c, a)| format!("{s:?} @{ms} ring {c:.2} ground agrees {a}"))
        .collect();
    assert!(under.is_empty(), "on {}: {under:?}", look.name);
    let ringless = read(|fx| fx.set_thin_caret_ring(false));
    let worst = ringless.iter().map(|r| r.2).fold(f64::INFINITY, f64::min);
    assert!(
        worst < CARET_MIN,
        "control: read against its cell alone, the bar's ring reads {worst:.2} at worst on {}",
        look.name
    );
}

#[test]
fn a_bar_caret_reads_against_its_ring_on_the_dark_page() {
    rings_read_on(DARK);
}

#[test]
fn a_bar_caret_reads_against_its_ring_on_paper() {
    rings_read_on(GITHUB_LIGHT);
}
