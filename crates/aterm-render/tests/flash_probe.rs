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
//! word icon's box: those are other laws' to hold (the pet's gait frames; a word icon's, held by
//! [`a_word_icon_never_flashes`]). A body's box holds the turns on the frame after it held the
//! pixel too: a sprite moving off a pixel turns it on the first frame its box no longer holds it.

use crate::trail_film::{Lane, Take, film_replay};
use crate::trail_letters_read::*;
use crate::wcag::luma;
use std::collections::HashMap;
use std::sync::mpsc::sync_channel;

/// The flagship, the dog's style, and the trail with the pet off.
const CAT: &str = "rainbow + cat";
const DOG: &str = "rainbow + dog";
const OFF: &str = "rainbow";
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

/// One place's reading of a film: its pixels over the bound, and the most turns one has in a
/// second with where it is.
#[derive(Debug)]
struct Read {
    over: usize,
    most: usize,
    worst: (usize, usize),
    frames: usize,
}

/// One film read by place, as `lab trail-film`'s `by_place` sorts its pixels: a pixel is the
/// place's that held the most of its busiest second's turns where those boxes held half of them
/// or more (the pet's body first, then the caret's cell, then a word icon's box), else the
/// trail's own. With how many frames held a word icon.
#[derive(Debug)]
struct Places {
    pet: Read,
    caret: Read,
    icon: Read,
    trail: Read,
    iconed: usize,
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
    flashes_by_place(style, take, look, caret).trail
}

/// A pixel-space box, end exclusive: `(x0, x1, y0, y1)`.
type PxBox = (i32, i32, i32, i32);

/// Film `take` in `style` on `look` under the caret `caret` sets and read its flashes by place.
fn flashes_by_place(style: &str, take: Take, look: Look, caret: &[u8]) -> Places {
    let (mut ev, tail) = take.events(look);
    ev.insert(0, (0, Ev::Out(caret.to_vec())));
    let (rows, cols) = take.grid();
    let end = ev.last().map_or(0, |e| e.0) + tail;
    let frames: Vec<u64> = (1..=end / STEP_MS).map(|k| k * STEP_MS).collect();
    let (to, kept) = sync_channel::<Kept>(2);
    let mut zig: Vec<Option<Zig>> = Vec::new();
    let mut turns: HashMap<usize, Vec<u64>> = HashMap::new();
    // Each frame's boxes (px, end exclusive): the pet's body and the flying head, which hold
    // the frame after theirs too; the caret's cell; and the word icons.
    let mut boxes: HashMap<u64, (Vec<PxBox>, PxBox, Vec<PxBox>)> = HashMap::new();
    let mut width = 1;
    let mut seen = 0;
    let mut iconed = 0;
    let (width, turns, boxes, seen, iconed) = std::thread::scope(|s| {
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
                let bodies: Vec<_> = g.pet_body.into_iter().chain(g.head_body).collect();
                iconed += usize::from(!g.word_icons.is_empty());
                boxes.insert(k.ms, (bodies, caret, g.word_icons.clone()));
                for (i, &p) in k.px.iter().enumerate() {
                    if turn(&mut zig[i], luma(p)) {
                        turns.entry(i).or_default().push(k.ms);
                    }
                }
                seen += 1;
            }
            (width, turns, boxes, seen, iconed)
        });
        // The replay runs on the caller's thread: a negative control's seam is that thread's.
        let mut r = film_replay(style, look, rows, cols, Lane::Paper);
        r.keep.clone_from(&frames);
        r.keep_to = Some(to);
        let _ = r.run(&ev, tail);
        drop(r);
        reader.join().expect("the film's reader")
    });
    let mut reads: [Read; 4] = std::array::from_fn(|_| Read {
        over: 0,
        most: 0,
        worst: (0, 0),
        frames: seen,
    });
    for (&i, times) in &turns {
        let (x, y) = ((i % width) as i32, (i / width) as i32);
        let (n, from) = most_within(times, 1_000);
        let holds = |&(x0, x1, y0, y1): &PxBox| x0 <= x && x < x1 && y0 <= y && y < y1;
        let mut held = [0usize; 3];
        for t in &times[from..from + n] {
            let Some((bodies, cell, icons)) = boxes.get(t) else {
                continue;
            };
            let left = t
                .checked_sub(STEP_MS)
                .and_then(|was| boxes.get(&was))
                .is_some_and(|(before, _, _)| before.iter().any(holds));
            if bodies.iter().any(holds) || left {
                held[0] += 1;
            } else if holds(cell) {
                held[1] += 1;
            } else if icons.iter().any(holds) {
                held[2] += 1;
            }
        }
        let boxed: usize = held.iter().sum();
        // The first of the most: `max_by_key` keeps the last, so the places are read from the end.
        let place = if 2 * boxed >= n {
            (0..3).rev().max_by_key(|&p| held[p]).unwrap_or(3)
        } else {
            3
        };
        let read = &mut reads[place];
        if n > read.most {
            (read.most, read.worst) = (n, (x as usize, y as usize));
        }
        read.over += usize::from(most_within(times, 1_000 - STEP_MS).0 > TURNS_MOST);
    }
    let [pet, caret, icon, trail] = reads;
    Places {
        pet,
        caret,
        icon,
        trail,
        iconed,
    }
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

/// Where a person types, on every scene of `lab trail-film` cheap enough for the dev set, the
/// trail's own pixels stay at three flashes a second or fewer: the flagship on paper, on a dark
/// page and on a dim one, under a bar, a block and an underline caret, and the dog's style.
/// Held: the scenes that read 0 on the films they are taken under here. Not held, each with
/// what still breaks it: `jump` under a bar caret and on paper (the row's ribbon and rail
/// washing and re-lighting under Home and End: 1 px on a dark page, 49 and 52 on GitHub Light;
/// a dark page's under a block caret is held), the walls on paper (`wall-70` and `wall-73` on
/// GitHub Light: the swoosh's suck-in slides the band's stepped top through the letter cells
/// whose top it lights, 15 px under a bar and 16 under a block, as on main since the band came
/// back beside the line; with no suck-in, 0), `enter-long` on paper (a flying
/// train's even stations), `backspace`, `steam`, `wrapped`, `focus-wrap`, `wall-69` and
/// paper's `fold-rest`; `verdict`, `last-row` and `long-build` are not cheap (twice to four
/// times the frames of the rest, and `long-build`'s page scrolls, which this reading does not
/// book apart).
///
/// What each control holds:
/// - the open row on GitHub Light (`typing`): paper's stars stand still and only cool. Its
///   control flies them as a dark page's fly (`stardust::with_paper_stars_flying`): the grains
///   of one typed line then cross the same pixels in turn, hundreds of pixels over the bound.
/// - a long line at rest in its fold on a dark page (`fold-rest`): the ribbon's head feather is
///   laid finely whenever its windows fit the quad budget. Its control lays it finely only in a
///   roomy frame (`ribbon::with_head_feather_by_room`), a stair of slabs that walks under the
///   feather as the line flows.
/// - the caret moved up a row on GitHub Light (`move-up`): a landed train's body stands on a
///   lattice from its landing. Its control spreads the body evenly over the span as the root
///   sucks in (`meteor::with_trains_spread`), every station sliding a little each frame.
/// - Home and End on a dark page (`jump`): a landing's sparks stand at points of their throws
///   and only cool. Its control throws them (`meteor::with_sparks_thrown`), forty to sixty
///   stars crossing the pixels round each landing in turn.
///
/// A film may sit at exactly six turns in a second, the bound itself: the law is the bound, and
/// a change that adds a turn there has broken it.
#[test]
fn the_flagship_trail_stays_under_three_flashes_a_second() {
    let g = Take::Gallery;
    let films: Vec<Film> = vec![
        ("typing", CAT, g(Scene::Typing), GITHUB_LIGHT, BAR),
        ("typing", CAT, g(Scene::Typing), DARK, BAR),
        ("typing", CAT, g(Scene::Typing), GITHUB_LIGHT, BLOCK),
        ("typing", DOG, g(Scene::Typing), GITHUB_LIGHT, BAR),
        ("burst", CAT, g(Scene::Burst), GITHUB_LIGHT, BAR),
        ("burst", CAT, g(Scene::Burst), DARK, BLOCK),
        ("burst", DOG, g(Scene::Burst), GITHUB_LIGHT, BLOCK),
        ("context", CAT, g(Scene::Context), DARK, BAR),
        ("context", CAT, g(Scene::Context), GITHUB_LIGHT, BAR),
        ("enter", CAT, g(Scene::EnterBottom), DARK, BAR),
        ("enter", DOG, g(Scene::EnterBottom), DARK, BLOCK),
        ("bottom-prompt", CAT, Take::BottomPrompt, GITHUB_LIGHT, BAR),
        ("bottom-prompt", CAT, Take::BottomPrompt, DARK, BAR),
        (
            "bottom-prompt",
            CAT,
            Take::BottomPrompt,
            GITHUB_LIGHT,
            BLOCK,
        ),
        (
            "bottom-prompt",
            CAT,
            Take::BottomPrompt,
            SOLARIZED_DARK,
            BAR,
        ),
        ("composer", CAT, Take::Composer, GITHUB_LIGHT, BAR),
        ("composer", CAT, Take::Composer, DARK, BAR),
        ("composer", CAT, Take::Composer, DARK, BLOCK),
        ("composer", CAT, Take::Composer, DARK, UNDERLINE),
        ("composer", DOG, Take::Composer, GITHUB_LIGHT, BAR),
        ("composer", DOG, Take::Composer, GITHUB_LIGHT, BLOCK),
        ("composer", DOG, Take::Composer, DARK, BLOCK),
        ("enter-long", CAT, Take::EnterLong, DARK, BAR),
        ("enter-long", DOG, Take::EnterLong, DARK, BLOCK),
        ("wall-73", CAT, Take::Wall(73), DARK, BAR),
        (
            "coloured-prompt",
            CAT,
            Take::ColouredPrompt,
            GITHUB_LIGHT,
            BAR,
        ),
        ("coloured-prompt", CAT, Take::ColouredPrompt, DARK, BAR),
        ("cjk", CAT, g(Scene::Cjk), GITHUB_LIGHT, BAR),
        ("cjk", DOG, g(Scene::Cjk), DARK, BLOCK),
        ("fold-rest", CAT, g(Scene::FoldRest), DARK, BAR),
        ("move-up", CAT, g(Scene::MoveUp), GITHUB_LIGHT, BAR),
        ("move-up", CAT, g(Scene::MoveUp), DARK, BAR),
        ("move-up", DOG, g(Scene::MoveUp), GITHUB_LIGHT, BLOCK),
        ("split", CAT, g(Scene::SplitBurst), GITHUB_LIGHT, BAR),
        ("split", DOG, g(Scene::SplitBurst), DARK, BAR),
        ("jump", CAT, Take::Jump, DARK, BLOCK),
    ];
    let (reads, flying, stair, spread, thrown) = std::thread::scope(|s| {
        let held: Vec<_> = films
            .iter()
            .map(|&(_, style, take, look, caret)| {
                s.spawn(move || trail_flashes(style, take, look, caret.1))
            })
            .collect();
        let flying = s.spawn(move || {
            aterm_effects::rainbow_kitty::stardust::with_paper_stars_flying(|| {
                trail_flashes(CAT, g(Scene::Typing), GITHUB_LIGHT, BAR.1)
            })
        });
        let stair = s.spawn(move || {
            aterm_effects::rainbow_kitty::ribbon::with_head_feather_by_room(|| {
                trail_flashes(CAT, g(Scene::FoldRest), DARK, BAR.1)
            })
        });
        let spread = s.spawn(move || {
            aterm_effects::rainbow_kitty::meteor::with_trains_spread(|| {
                trail_flashes(CAT, g(Scene::MoveUp), GITHUB_LIGHT, BAR.1)
            })
        });
        let thrown = s.spawn(move || {
            aterm_effects::rainbow_kitty::meteor::with_sparks_thrown(|| {
                trail_flashes(CAT, Take::Jump, DARK, BLOCK.1)
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
            spread.join().expect("the trains' control"),
            thrown.join().expect("the sparks' control"),
        )
    });
    // Every film is read before any is judged, so one red names all of them.
    let mut reds = Vec::new();
    for ((scene, style, _, look, caret), read) in films.iter().zip(&reads) {
        let film = format!("{style}, {scene} on {}, {} caret", look.name, caret.0);
        eprintln!("flash_probe {film}: {read:?}");
        if read.frames <= 100 {
            reds.push(format!("{film} filmed {} frames", read.frames));
        } else if read.over != 0 {
            reds.push(format!(
                "{film}: {} px of the trail's own flash faster than 3 a second, the worst {} \
                 turns in a second at px {:?}",
                read.over, read.most, read.worst
            ));
        }
    }
    eprintln!("flash_probe control (paper's stars flying, typing, github-light): {flying:?}");
    eprintln!("flash_probe control (the head feather laid by room, fold-rest, dark): {stair:?}");
    eprintln!("flash_probe control (a landed train spread, move-up, github-light): {spread:?}");
    eprintln!("flash_probe control (a landing's sparks thrown, jump, dark): {thrown:?}");
    assert!(reds.is_empty(), "{}", reds.join("\n"));
    assert!(
        flying.over > 100 && flying.most > 2 * TURNS_MOST,
        "control: with paper's stars flying the open row reads {} px over and {} turns at \
         most, so the law is not reading the stars",
        flying.over,
        flying.most
    );
    assert!(
        stair.over > 10 && stair.most > 2 * TURNS_MOST,
        "control: with the head feather laid by room the fold reads {} px over and {} turns \
         at most, so the law is not reading the feather",
        stair.over,
        stair.most
    );
    assert!(
        spread.over > 10 && spread.most > TURNS_MOST,
        "control: with a landed train's body spread the move up reads {} px over and {} \
         turns at most, so the law is not reading the train",
        spread.over,
        spread.most
    );
    assert!(
        thrown.over > 10 && thrown.most > TURNS_MOST,
        "control: with a landing's sparks thrown Home and End read {} px over and {} turns \
         at most, so the law is not reading the sparks",
        thrown.over,
        thrown.most
    );
}

/// CT-48: a word icon never flashes. A sparkle word's icon (a fox's, a dog's,
/// a cat's face peeking from behind its word) is drawn whole at its rest box
/// on every frame it shows and comes and goes by its alpha alone, so no pixel
/// of an icon's box turns more than six times in a second: before, its art
/// slid over the glass in whole-pixel steps (the rise with its overshoot, the
/// breathing bob and sway, the descent) and put 487 to 1439 px of each burst
/// and composer film over the bound. Held with the pet off, where the icons
/// are the only sprites on the row: the burst on paper under a block caret and
/// on a dark page under a bar, Return on paper, and the composer on a dark page
/// and on paper. The pet's films are read and printed, not held here: what
/// the pet does to an icon (its claim, its stand on the span, its bat cue) is
/// the pet's own package.
///
/// The control restores the dwell pose that shipped before
/// (`word_decorations::with_icons_breathing`): a dwelling icon's 1-2 px bob
/// and sway alone put the burst's icons over the bound again.
#[test]
fn a_word_icon_never_flashes() {
    let g = Take::Gallery;
    let held: Vec<Film> = vec![
        ("burst", OFF, g(Scene::Burst), GITHUB_LIGHT, BLOCK),
        ("burst", OFF, g(Scene::Burst), DARK, BAR),
        ("enter", OFF, g(Scene::EnterBottom), GITHUB_LIGHT, BAR),
        ("composer", OFF, Take::Composer, DARK, BLOCK),
        ("composer", OFF, Take::Composer, GITHUB_LIGHT, BAR),
    ];
    let read: Vec<Film> = vec![
        ("burst", CAT, g(Scene::Burst), GITHUB_LIGHT, BLOCK),
        ("composer", DOG, Take::Composer, GITHUB_LIGHT, BAR),
    ];
    let (reads, pets, breathing) = std::thread::scope(|s| {
        let film = |&(_, style, take, look, caret): &Film| {
            s.spawn(move || flashes_by_place(style, take, look, caret.1))
        };
        let held: Vec<_> = held.iter().map(film).collect();
        let pets: Vec<_> = read.iter().map(film).collect();
        let breathing = s.spawn(move || {
            aterm_effects::word_decorations::with_icons_breathing(|| {
                flashes_by_place(OFF, g(Scene::Burst), GITHUB_LIGHT, BLOCK.1)
            })
        });
        let join = |h: Vec<std::thread::ScopedJoinHandle<'_, Places>>| -> Vec<Places> {
            h.into_iter().map(|h| h.join().expect("a film")).collect()
        };
        (
            join(held),
            join(pets),
            breathing.join().expect("the breathing control"),
        )
    });
    let mut reds = Vec::new();
    for ((scene, style, _, look, caret), read) in held.iter().zip(&reads) {
        let film = format!("{style}, {scene} on {}, {} caret", look.name, caret.0);
        eprintln!("flash_probe icons {film}: {read:?}");
        let iconed = *scene != "enter";
        if read.icon.frames <= 100 {
            reds.push(format!("{film} filmed {} frames", read.icon.frames));
        } else if iconed && read.iconed <= 100 {
            reds.push(format!("{film} showed icons in {} frames", read.iconed));
        } else if read.icon.over != 0 {
            reds.push(format!(
                "{film}: {} px of the word icons flash faster than 3 a second, the worst {} \
                 turns in a second at px {:?}",
                read.icon.over, read.icon.most, read.icon.worst
            ));
        }
    }
    for ((scene, style, _, look, caret), read) in read.iter().zip(&pets) {
        let film = format!("{style}, {scene} on {}, {} caret", look.name, caret.0);
        eprintln!(
            "flash_probe icons (read, not held) {film}: px over the bound by place: pet {}, \
             caret {}, icon {} (worst {} turns at {:?}), trail {}; icons in {} of {} frames",
            read.pet.over,
            read.caret.over,
            read.icon.over,
            read.icon.most,
            read.icon.worst,
            read.trail.over,
            read.iconed,
            read.icon.frames
        );
    }
    eprintln!(
        "flash_probe icons control (the dwell breathing, burst, github-light): {breathing:?}"
    );
    assert!(reds.is_empty(), "{}", reds.join("\n"));
    assert!(
        breathing.icon.over > 10 && breathing.icon.most > TURNS_MOST,
        "control: with the icons breathing in their dwell the burst reads {} px of the icons' \
         over and {} turns at most, so the law is not reading the icons",
        breathing.icon.over,
        breathing.icon.most
    );
}

/// The Default Light block Backspace film's pixels still over the bound (ATERM-KI-1114-262).
const DEFAULT_LIGHT_BLOCK_KNOWN: usize = 5;

/// Held Backspace on a light page keeps the trail under three flashes a second, block caret and
/// bar (`lab trail-film`'s `backspace` scene with the pet off, on GitHub Light, Default Light and
/// Paper Grey): no pixel of the trail's own is over the bound.
///
/// Three lights turned there. A Backspace whose echo shared its engine tick laid a fresh cell
/// under the caret it moved back onto, and the rail and band there dropped to the page for a
/// frame and came back (`rainbow_kitty`'s `erase_tail_start`); its control replays that order as
/// it was (`with_same_tick_erase_as_was`). The block caret's innermost halo ring hugged the
/// block one pixel out, an outline that left its old cell and lit its new one on every
/// Backspace; it is drawn inside the block over the caret's fill now, as a keyline that moves
/// with the caret's own pixels (`CursorFxFrame::keyline_tint`), and its control draws it outside
/// again (`with_block_rim_outside`). And the block's outer rings, whose reach follows the paint
/// a key lifts, stepped a pixel in a frame or two after each key and out again at the next: on
/// Default Light's pale band a turn pair beside the caret at every key; their reach is held at
/// the caret's cell now (`cursor_rainbow`'s `RingHold`), and their control lets it follow the
/// paint again (`with_halo_rings_unheld`).
///
/// Not held yet: Default Light under the block, where 5 px still turn 8 times in a second
/// (ATERM-KI-1114-262, the outer ring beside the caret on the pale band), so that film is held
/// to those 5 and the unheld rings' control to more than them.
#[test]
fn a_light_page_s_backspace_stays_under_three_flashes_a_second() {
    const STYLE: &str = "rainbow";
    let take = Take::Backspace;
    let films = [GITHUB_LIGHT, DEFAULT_LIGHT, PAPER_GREY]
        .into_iter()
        .flat_map(|look| [(look, BLOCK), (look, BAR)]);
    let (reads, same_tick, rim_outside, unheld) = std::thread::scope(|s| {
        let reads: Vec<_> = films
            .map(|(look, caret)| {
                (
                    look.name,
                    caret.0,
                    s.spawn(move || trail_flashes(STYLE, take, look, caret.1)),
                )
            })
            .collect();
        let same_tick = s.spawn(move || {
            aterm_effects::rainbow_kitty::with_same_tick_erase_as_was(|| {
                trail_flashes(STYLE, take, GITHUB_LIGHT, BAR.1)
            })
        });
        let rim_outside = s.spawn(move || {
            aterm_effects::cursor_fx::with_block_rim_outside(|| {
                trail_flashes(STYLE, take, GITHUB_LIGHT, BLOCK.1)
            })
        });
        let unheld = s.spawn(move || {
            aterm_effects::cursor_rainbow::with_halo_rings_unheld(|| {
                trail_flashes(STYLE, take, DEFAULT_LIGHT, BLOCK.1)
            })
        });
        (
            reads
                .into_iter()
                .map(|(look, caret, j)| (look, caret, j.join().expect("a film")))
                .collect::<Vec<_>>(),
            same_tick.join().expect("the same-tick control"),
            rim_outside.join().expect("the rim's control"),
            unheld.join().expect("the rings' control"),
        )
    });
    for (look, caret, read) in &reads {
        eprintln!("flash_probe backspace on {look}, {caret} caret: {read:?}");
    }
    eprintln!("flash_probe control (the same-tick Backspace as it was, light bar): {same_tick:?}");
    eprintln!("flash_probe control (the block's rim outside it, light block): {rim_outside:?}");
    eprintln!("flash_probe control (the rings' reach unheld, default-light block): {unheld:?}");
    for (look, caret, read) in &reads {
        assert!(
            read.frames > 100,
            "the {look} {caret} film filmed {} frames",
            read.frames
        );
        let known = if (*look, *caret) == (DEFAULT_LIGHT.name, BLOCK.0) {
            DEFAULT_LIGHT_BLOCK_KNOWN
        } else {
            0
        };
        assert!(
            read.over <= known,
            "backspace on {look}, {caret} caret: {} px of the trail's own flash faster than 3 a \
             second, the worst {} turns in a second at px {:?}",
            read.over,
            read.most,
            read.worst
        );
    }
    assert!(
        same_tick.over > 10 && same_tick.most > TURNS_MOST,
        "control: with the same-tick Backspace as it was the bar film reads {} px over and {} \
         turns at most, so the law is not reading the rail's dip",
        same_tick.over,
        same_tick.most
    );
    assert!(
        rim_outside.over > 0 && rim_outside.most > TURNS_MOST,
        "control: with the block's rim outside it the block film reads {} px over and {} turns \
         at most, so the law is not reading the rim",
        rim_outside.over,
        rim_outside.most
    );
    assert!(
        unheld.over > DEFAULT_LIGHT_BLOCK_KNOWN && unheld.most > TURNS_MOST,
        "control: with the rings' reach unheld the Default Light block film reads {} px over and \
         {} turns at most, so the law is not reading the rings' step",
        unheld.over,
        unheld.most
    );
}
