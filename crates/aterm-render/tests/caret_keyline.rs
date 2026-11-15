// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rainbow block caret's keyline on glass: the one-pixel ring just inside the block that its
//! innermost halo ring became (`aterm_render::block_keyline_colour`), read off the films of the
//! shipping effects and the real renderer ([`film_replay`]) on every film theme.

use crate::trail_film::{Lane, Take, film_replay};
use crate::trail_letters_read::*;
use aterm_core::terminal::CursorStyle;
use std::sync::mpsc::sync_channel;

/// The six film themes.
const LOOKS: [Look; 6] = [
    DARK,
    GITHUB_LIGHT,
    DEFAULT_LIGHT,
    DIM_GREY,
    SOLARIZED_DARK,
    PAPER_GREY,
];

/// A film's frame step, ms.
const STEP_MS: u64 = 16;

/// The 8-bit rounding a pixel read may lose against a floor.
const ROUNDING: f32 = 0.99;

fn contrast(a: u32, b: u32) -> f32 {
    crate::wcag::contrast(a, b) as f32
}

/// The worst keyline a film of `take` on `look` under the rainbow block puts on glass: the least
/// contrast of an edge pixel of the block against the page (relative to the fill's own, where
/// that is under the caret's floor) and against the fill beside it, each with its frame (ms); and
/// how many frames wore a keyline.
#[derive(Debug, Clone, Copy)]
struct Worst {
    page: (f32, u64),
    fill: (f32, u64),
    /// The worst fill reading's edge and fill pixels, and the frame's caret cell.
    fill_px: (u32, u32, (usize, usize)),
    frames: usize,
}

fn worst_keyline(take: Take, look: Look) -> Worst {
    let (ev, tail) = take.events(look);
    let (rows, cols) = take.grid();
    let end = ev.last().map_or(0, |e| e.0) + tail;
    let frames: Vec<u64> = (1..=end / STEP_MS).map(|k| k * STEP_MS).collect();
    let (to, kept) = sync_channel::<Kept>(2);
    std::thread::scope(|s| {
        let reader = s.spawn(move || {
            let mut w = Worst {
                page: (f32::MAX, 0),
                fill: (f32::MAX, 0),
                fill_px: (0, 0, (0, 0)),
                frames: 0,
            };
            for k in kept.iter() {
                let block = matches!(
                    k.glass.caret_style,
                    CursorStyle::BlinkingBlock | CursorStyle::SteadyBlock
                );
                if k.keyline_tint.is_none() || !k.glass.caret_visible || !block {
                    continue;
                }
                w.frames += 1;
                let (cw, ch) = k.cell;
                let x0 = k.left + k.caret.1 * cw;
                let y0 = k.top + k.caret.0 * ch;
                let at = |x: usize, y: usize| k.px[y * k.w + x];
                // The fill just inside the keyline's top-left corner, and the keyline's pixels
                // beside it on the top and left edges.
                let fill = at(x0 + 1, y0 + 1);
                let page_floor = 4.5f32.min(contrast(fill, look.bg));
                for edge in [
                    at(x0 + 1, y0),
                    at(x0, y0 + 1),
                    at(x0 + cw / 2, y0),
                    at(x0, y0 + ch / 2),
                ] {
                    let p = contrast(edge, look.bg) / page_floor;
                    if p < w.page.0 {
                        w.page = (p, k.ms);
                    }
                    let f = contrast(edge, fill);
                    if f < w.fill.0 {
                        w.fill = (f, k.ms);
                        w.fill_px = (edge, fill, k.caret);
                    }
                }
            }
            w
        });
        // The replay runs on the caller's thread: a negative control's seam is that thread's.
        let mut r = film_replay("rainbow", look, rows, cols, Lane::Paper);
        r.keep.clone_from(&frames);
        r.keep_to = Some(to);
        let _ = r.run(&ev, tail);
        drop(r);
        reader.join().expect("the film's reader")
    })
}

/// On every film theme, through the burst and the held Backspace, the rainbow block's keyline
/// reads the caret's floor against the page (4.5:1, or the fill's own where that is less) and
/// [`aterm_render::KEYLINE_GLASS_MIN_CONTRAST`] against the fill it outlines, under the light
/// laid over the caret after it. The control draws
/// the ring's tint over the fill and nothing more, as the keyline was first drawn
/// (`aterm_render::with_keyline_as_tinted`): 3.5:1 against GitHub Light's page in the burst, and
/// all but lost on Dark's light block.
#[test]
fn the_rainbow_block_s_keyline_reads_on_every_theme() {
    let takes = [Take::Gallery(Scene::Burst), Take::Backspace];
    let reads: Vec<(&str, usize, Worst)> = std::thread::scope(|s| {
        let jobs: Vec<_> = LOOKS
            .iter()
            .flat_map(|&look| {
                takes
                    .iter()
                    .enumerate()
                    .map(move |(t, &take)| (look, t, take))
            })
            .map(|(look, t, take)| (look.name, t, s.spawn(move || worst_keyline(take, look))))
            .collect();
        jobs.into_iter()
            .map(|(name, t, j)| (name, t, j.join().expect("a film")))
            .collect()
    });
    let mut bad = Vec::new();
    for (name, t, w) in &reads {
        let take = ["burst", "backspace"][*t];
        eprintln!("caret keyline {name} {take}: {w:?}");
        if w.frames < 20 {
            bad.push(format!(
                "{name} {take}: fixture, {} frames wore a keyline",
                w.frames
            ));
        }
        if w.page.0 < ROUNDING {
            bad.push(format!(
                "{name} {take}: the keyline reads {:.2} of the caret's floor on the page at {} ms",
                w.page.0, w.page.1
            ));
        }
        if w.fill.0 < aterm_render::KEYLINE_GLASS_MIN_CONTRAST * ROUNDING {
            bad.push(format!(
                "{name} {take}: the keyline reads {:.2}:1 on the fill at {} ms",
                w.fill.0, w.fill.1
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
    let (light, dark) = std::thread::scope(|s| {
        let light = s.spawn(|| {
            aterm_render::with_keyline_as_tinted(|| {
                worst_keyline(Take::Gallery(Scene::Burst), GITHUB_LIGHT)
            })
        });
        let dark = s.spawn(|| {
            aterm_render::with_keyline_as_tinted(|| {
                worst_keyline(Take::Gallery(Scene::Burst), DARK)
            })
        });
        (
            light.join().expect("the light control"),
            dark.join().expect("the dark control"),
        )
    });
    eprintln!("control (tint alone) light burst: {light:?}; dark burst: {dark:?}");
    assert!(
        light.page.0 < ROUNDING,
        "control: the tint alone reads {:.2} of the caret's floor on GitHub Light's page",
        light.page.0
    );
    assert!(
        dark.fill.0 < aterm_render::KEYLINE_GLASS_MIN_CONTRAST * ROUNDING,
        "control: the tint alone reads {:.2}:1 on Dark's fill",
        dark.fill.0
    );
}
