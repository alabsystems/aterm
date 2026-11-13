// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The flash law on the rainbow trail, as a check: no pixel of the trail's own turns more than
//! three flashes a second where a person types, in a film of the shipping effects and the real
//! renderer ([`film_replay`], the film `lab trail-film` judges).
//!
//! The reading is `lab trail-film`'s FLASH law (`aterm-lab`'s `trail_film::Turns` and
//! `by_place`), held here on the frames in memory: a turn is a change of a pixel's WCAG
//! relative luminance of a tenth or more from its last extreme whose darker state is under
//! 0.80; two opposing turns are a flash; a pixel is over the bound with more than six turns in
//! a second less one frame step. A pixel is the trail's own when fewer than half the turns of
//! its busiest second fell in the pet's drawn box, the flying head's, the caret's cell or a
//! word icon's box: those are other laws' to hold (the pet's gait frames, the icon's pop). A
//! body's box holds the turns on the frame after it held the pixel too: a sprite moving off a
//! pixel turns it on the first frame its box no longer holds it.

use crate::trail_film::{Take, film_replay};
use crate::trail_letters_read::*;
use crate::wcag::luma;
use std::collections::HashMap;
use std::sync::mpsc::sync_channel;

/// The flagship, and the dog's style.
const CAT: &str = "rainbow + cat";
const DOG: &str = "rainbow + dog";
/// The carets a film is taken under, each with the DECSCUSR that sets it.
const BLOCK: (&str, &[u8]) = ("block", b"");
const BAR: (&str, &[u8]) = ("bar", b"\x1b[6 q");
const UNDERLINE: (&str, &[u8]) = ("underline", b"\x1b[4 q");
/// A film's frame step, ms.
const STEP_MS: u64 = 16;
/// The least change of relative luminance that is a turn (WCAG 2.3.1: 10% of white).
const SWING: f64 = 0.10;
/// A turn's darker state is under this.
const DARK_BELOW: f64 = 0.80;
/// Turns in a second at the bound: three flashes.
const TURNS_MOST: usize = 6;

/// One film's trail: the pixels of its own over the bound, and the most turns one has in a
/// second with where it is.
#[derive(Debug)]
struct Read {
    over: usize,
    most: usize,
    worst: (usize, usize),
    frames: usize,
}

/// A pixel's turns, as the judge's `Turns` counts them.
#[derive(Clone, Copy)]
struct Zig {
    dir: i8,
    lo: f64,
    hi: f64,
}

/// The most of `times` (sorted, ms) within `window` ms of one of them, and where that run starts.
fn most_within(times: &[u64], window: u64) -> (usize, usize) {
    let mut best = (0, 0);
    for (i, &t) in times.iter().enumerate() {
        let n = times[i..].iter().take_while(|&&u| u < t + window).count();
        if n > best.0 {
            best = (n, i);
        }
    }
    best
}

/// Film `take` in `style` on `look` under the caret `caret` sets and read the trail's own
/// flashes.
fn trail_flashes(style: &str, take: Take, look: Look, caret: &[u8]) -> Read {
    let (mut ev, tail) = take.events(look);
    ev.insert(0, (0, Ev::Out(caret.to_vec())));
    let (rows, cols) = take.grid();
    let end = ev.last().map_or(0, |e| e.0) + tail;
    let frames: Vec<u64> = (1..=end / STEP_MS).map(|k| k * STEP_MS).collect();
    let (to, kept) = sync_channel::<Kept>(2);
    let mut zig: Vec<Option<Zig>> = Vec::new();
    let mut turns: HashMap<usize, Vec<u64>> = HashMap::new();
    // Each frame's boxes (px, end exclusive): the pet's body, the flying head, the caret's
    // cell and the word icons; and its bodies alone, which hold the frame after theirs too.
    let mut boxes: HashMap<u64, Vec<(i32, i32, i32, i32)>> = HashMap::new();
    let mut bodies: HashMap<u64, Vec<(i32, i32, i32, i32)>> = HashMap::new();
    let mut width = 1;
    let mut seen = 0;
    let (width, turns, boxes, bodies, seen) = std::thread::scope(|s| {
        let reader = s.spawn(move || {
            for k in kept.iter() {
                width = k.w;
                zig.resize(k.px.len(), None);
                let (cw, ch) = k.cell;
                let caret = (
                    (k.left + k.caret.1 * cw) as i32,
                    (k.left + (k.caret.1 + 1) * cw) as i32,
                    (k.top + k.caret.0 * ch) as i32,
                    (k.top + (k.caret.0 + 1) * ch) as i32,
                );
                let g = &k.glass;
                let mut here: Vec<_> = g.pet_body.into_iter().chain(g.head_body).collect();
                bodies.insert(k.ms, here.clone());
                here.push(caret);
                here.extend(g.word_icons.iter().copied());
                boxes.insert(k.ms, here);
                for (i, &p) in k.px.iter().enumerate() {
                    if turn(&mut zig[i], luma(p)) {
                        turns.entry(i).or_default().push(k.ms);
                    }
                }
                seen += 1;
            }
            (width, turns, boxes, bodies, seen)
        });
        // The replay runs on the caller's thread: a negative control's seam is that thread's.
        let mut r = film_replay(style, look, rows, cols);
        r.keep.clone_from(&frames);
        r.keep_to = Some(to);
        let _ = r.run(&ev, tail);
        drop(r);
        reader.join().expect("the film's reader")
    });
    let mut read = Read {
        over: 0,
        most: 0,
        worst: (0, 0),
        frames: seen,
    };
    for (&i, times) in &turns {
        let (x, y) = ((i % width) as i32, (i / width) as i32);
        let (n, from) = most_within(times, 1_000);
        let holds = |b: &Vec<(i32, i32, i32, i32)>| {
            b.iter()
                .any(|&(x0, x1, y0, y1)| x0 <= x && x < x1 && y0 <= y && y < y1)
        };
        let boxed = times[from..from + n]
            .iter()
            .filter(|&&t| {
                boxes.get(&t).is_some_and(holds)
                    || t.checked_sub(STEP_MS)
                        .and_then(|was| bodies.get(&was))
                        .is_some_and(holds)
            })
            .count();
        if 2 * boxed >= n {
            continue;
        }
        if n > read.most {
            (read.most, read.worst) = (n, (x as usize, y as usize));
        }
        read.over += usize::from(most_within(times, 1_000 - STEP_MS).0 > TURNS_MOST);
    }
    read
}

/// Feed a pixel's next luminance to its zigzag; true when it turned.
fn turn(zig: &mut Option<Zig>, v: f64) -> bool {
    let Some(z) = zig else {
        *zig = Some(Zig {
            dir: 0,
            lo: v,
            hi: v,
        });
        return false;
    };
    let turned = match z.dir {
        0 => {
            let was_lo = z.lo;
            (z.lo, z.hi) = (z.lo.min(v), z.hi.max(v));
            if z.hi - z.lo >= SWING && z.lo < DARK_BELOW {
                if v > was_lo { 1 } else { -1 }
            } else {
                0
            }
        }
        1 if v > z.hi => {
            z.hi = v;
            0
        }
        1 if z.hi - v >= SWING && v < DARK_BELOW => -1,
        -1 if v < z.lo => {
            z.lo = v;
            0
        }
        -1 if v - z.lo >= SWING && z.lo < DARK_BELOW => 1,
        _ => 0,
    };
    if turned != 0 {
        *z = Zig {
            dir: turned,
            lo: v,
            hi: v,
        };
    }
    turned != 0
}

/// One film of the check: its name, style, scene, theme and caret.
type Film = (
    &'static str,
    &'static str,
    Take,
    Look,
    (&'static str, &'static [u8]),
);

/// Where a person types (a shell's prompt on the last row of a full page, Claude Code's
/// composer) and on an open row, the trail's own pixels stay at three flashes a second or
/// fewer: the flagship on paper, on a dark page and on a dim one, under a bar, a block and an
/// underline caret, and the dog's style where its own residue does not already break the law
/// (its dark films under a bar caret read 1 and 15 px over at the caret, not held here).
///
/// What each film holds:
/// - the open row on GitHub Light (`typing`): paper's stars stand still and only cool. Its
///   control flies them as a dark page's fly (`stardust::with_paper_stars_flying`): the grains
///   of one typed line then cross the same pixels in turn, hundreds of pixels over the bound.
/// - a long line at rest in its fold on a dark page (`fold-rest`): the ribbon's head feather is
///   laid finely whenever its windows fit the quad budget. Its control lays it finely only in a
///   roomy frame (`ribbon::with_head_feather_by_room`), a stair of slabs that walks under the
///   feather as the line flows.
/// - the resident scenes: the line in the typed row's foot band, its gaps and the stars over
///   it add no pixel over the bound, in either style. No control of their own; they read 0
///   before the line was drawn there too.
///
/// A film may sit at exactly six turns in a second, the bound itself (the open row and the
/// fold do): the law is the bound, and a change that adds a turn there has broken it.
#[test]
fn the_flagship_trail_stays_under_three_flashes_a_second() {
    let typing = Take::Gallery(Scene::Typing);
    let fold = Take::Gallery(Scene::FoldRest);
    let films: [Film; 13] = [
        ("bottom-prompt", CAT, Take::BottomPrompt, GITHUB_LIGHT, BAR),
        ("bottom-prompt", CAT, Take::BottomPrompt, DARK, BAR),
        ("composer", CAT, Take::Composer, GITHUB_LIGHT, BAR),
        ("composer", CAT, Take::Composer, DARK, BAR),
        ("typing", CAT, typing, GITHUB_LIGHT, BAR),
        ("fold-rest", CAT, fold, DARK, BAR),
        (
            "bottom-prompt",
            CAT,
            Take::BottomPrompt,
            GITHUB_LIGHT,
            BLOCK,
        ),
        ("composer", CAT, Take::Composer, DARK, BLOCK),
        ("composer", CAT, Take::Composer, DARK, UNDERLINE),
        (
            "bottom-prompt",
            CAT,
            Take::BottomPrompt,
            SOLARIZED_DARK,
            BAR,
        ),
        ("composer", DOG, Take::Composer, GITHUB_LIGHT, BAR),
        ("composer", DOG, Take::Composer, GITHUB_LIGHT, BLOCK),
        ("composer", DOG, Take::Composer, DARK, BLOCK),
    ];
    let (reads, flying, stair) = std::thread::scope(|s| {
        let held: Vec<_> = films
            .iter()
            .map(|&(_, style, take, look, caret)| {
                s.spawn(move || trail_flashes(style, take, look, caret.1))
            })
            .collect();
        let flying = s.spawn(move || {
            aterm_effects::rainbow_kitty::stardust::with_paper_stars_flying(|| {
                trail_flashes(CAT, typing, GITHUB_LIGHT, BAR.1)
            })
        });
        let stair = s.spawn(move || {
            aterm_effects::rainbow_kitty::ribbon::with_head_feather_by_room(|| {
                trail_flashes(CAT, fold, DARK, BAR.1)
            })
        });
        let reads: Vec<Read> = held
            .into_iter()
            .map(|h| h.join().expect("a film"))
            .collect();
        (
            reads,
            flying.join().expect("the stars' control"),
            stair.join().expect("the feather's control"),
        )
    });
    for ((scene, style, _, look, caret), read) in films.iter().zip(&reads) {
        let film = format!("{style}, {scene} on {}, {} caret", look.name, caret.0);
        eprintln!("flash_probe {film}: {read:?}");
        assert!(read.frames > 100, "{film} filmed {} frames", read.frames);
        assert_eq!(
            read.over, 0,
            "{film}: {} px of the trail's own flash faster than 3 a second, the worst {} \
             turns in a second at px {:?}",
            read.over, read.most, read.worst
        );
    }
    eprintln!("flash_probe control (paper's stars flying, typing, github-light): {flying:?}");
    assert!(
        flying.over > 100 && flying.most > 2 * TURNS_MOST,
        "control: with paper's stars flying the open row reads {} px over and {} turns at \
         most, so the law is not reading the stars",
        flying.over,
        flying.most
    );
    eprintln!("flash_probe control (the head feather laid by room, fold-rest, dark): {stair:?}");
    assert!(
        stair.over > 10 && stair.most > 2 * TURNS_MOST,
        "control: with the head feather laid by room the fold reads {} px over and {} turns \
         at most, so the law is not reading the feather",
        stair.over,
        stair.most
    );
}
