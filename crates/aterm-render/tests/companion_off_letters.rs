// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The companion keeps off the letters: the trail gallery's scenes replayed through the shipping
// effects pipeline and the real renderer (`trail_letters_read`'s replay), every 16 ms frame, the
// pet's drawn body (the knock layer's rects that meet its box) read against every lettered
// cell's ink, the caret's cell and the word icons on glass.

use crate::trail_letters_read::{
    Coat, DARK, DEFAULT_LIGHT, Ev, GITHUB_LIGHT, Judged, Look, Replay, SOLARIZED_DARK, Scene,
    TYPING_TEXT, typed,
};
use aterm_effects::cursor_fx::TrailColor;
use aterm_effects::kitty_pet::OldPath;
use std::ops::RangeInclusive;

/// The companion styles.
pub const COMPANIONS: [&str; 4] = [
    "rainbow kitty pet",
    "rainbow kitty underline",
    "rainbow dog",
    "flat rainbow",
];

/// How a replay's keys reach the terminal and when its frames render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drive {
    /// Through the console, as a window's keys, a frame every 16 ms.
    Console,
    /// Past the console, as `aterm ctl key` types them ([`Replay::keys_unnoted`]).
    Ctl,
    /// Past the console and on the trail gallery's own frames ([`Scene::gallery_frames`]),
    /// as the gallery drives its headless instance.
    Gallery,
}

/// One replay of `style` on `look` through `scene`, every frame's coat read, its keys and
/// frames driven as `drive` says; `old` names the laws the companion's negative control
/// reverts ([`aterm_effects::EffectsPipeline::set_pet_old_path`]).
pub fn coats_driven(
    style: &str,
    look: Look,
    scene: Scene,
    old: &[OldPath],
    drive: Drive,
) -> Vec<Judged> {
    let mut r = Replay::sized(style, look, TrailColor::Automatic, scene.rows());
    r.coat = true;
    r.keys_unnoted = drive != Drive::Console;
    if drive == Drive::Gallery {
        r.frames_at = scene.gallery_frames();
        assert!(
            r.frames_at.is_some(),
            "fixture: {scene:?} has the gallery's frames"
        );
    }
    // The word pass on, as the app's default config has it: its icons on glass and the
    // page's ink it reads for the companion.
    r.fx.set_sparkle_enabled(true);
    for &law in old {
        r.fx.set_pet_old_path(law, true);
    }
    if let (Ok(dir), Ok(ms)) = (std::env::var("COAT_DUMP"), std::env::var("COAT_DUMP_MS")) {
        for f in ms.split(',').filter_map(|v| v.parse::<u64>().ok()) {
            r.dump.push((
                f,
                format!(
                    "{dir}/{}-{}-{scene:?}{}-{f:05}.png",
                    style.replace(' ', "-"),
                    look.name,
                    if old.is_empty() { "" } else { "-old" }
                ),
            ));
        }
    }
    let (ev, tail) = scene.events();
    r.run(&ev, tail)
}

/// A replay's sum: frames whose body lay on ink, the ink px, the worst frame, frames on the
/// caret, frames on an icon, and how often an icon on glass went missing for a frame
/// and came back (`blinks`).
#[derive(Debug, Default)]
pub struct Sum {
    pub frames: usize,
    pub on_ink: usize,
    pub ink_px: usize,
    pub worst: (u64, usize),
    pub on_caret: usize,
    pub on_icon: usize,
    pub blinks: usize,
}

pub fn sum(judged: &[Judged], from_ms: u64) -> Sum {
    let mut s = Sum::default();
    let mut icons_seen: Vec<usize> = Vec::new();
    for j in judged.iter().filter(|j| j.ms >= from_ms) {
        let c: &Coat = &j.coat;
        s.frames += 1;
        if c.ink_px > 0 {
            s.on_ink += 1;
            s.ink_px += c.ink_px;
            if c.ink_px > s.worst.1 {
                s.worst = (j.ms, c.ink_px);
            }
        }
        s.on_caret += usize::from(c.caret_px > 0);
        s.on_icon += usize::from(c.icon_px > 0);
        icons_seen.push(c.icons.len());
    }
    for w in icons_seen.windows(3) {
        if w[1] < w[0] && w[2] > w[1] {
            s.blinks += 1;
        }
    }
    s
}

/// A survey, printed: every companion style on dark, GitHub Light, Default Light and Solarized
/// Dark through the burst, the split's burst and the upward move.
#[test]
#[ignore = "a survey: run by hand to read where the companion lies"]
fn survey_where_the_companion_lies() {
    let only = std::env::var("COAT_ONLY").unwrap_or_default();
    for scene in [
        Scene::Burst,
        Scene::SplitBurst,
        Scene::MoveUp,
        Scene::MoveUpHasty,
    ] {
        for look in [DARK, GITHUB_LIGHT, DEFAULT_LIGHT, SOLARIZED_DARK] {
            for style in COMPANIONS {
                let key = format!("{style}/{}/{scene:?}", look.name);
                if !only.is_empty() && !key.contains(&only) {
                    continue;
                }
                let drive = match std::env::var("COAT_DRIVE").as_deref() {
                    Ok("ctl") => Drive::Ctl,
                    Ok("gallery") => Drive::Gallery,
                    _ => Drive::Console,
                };
                if drive == Drive::Gallery && scene.gallery_frames().is_none() {
                    continue;
                }
                let old: &[OldPath] = if std::env::var_os("COAT_OLD").is_some() {
                    &OldPath::EVERY
                } else {
                    &[]
                };
                let judged = coats_driven(style, look, scene, old, drive);
                let s = sum(&judged, 0);
                println!("{style:24} {:15} {scene:?}: {s:?}", look.name);
                if let Ok(frames) = std::env::var("COAT_FRAMES") {
                    for j in &judged {
                        let c = &j.coat;
                        if frames == "all" || c.ink_px > 0 || c.icon_px > 0 || c.caret_px > 0 {
                            println!(
                                "  {:5} ms pet {:?} ink {} caret {} icon {} cells {:?}",
                                j.ms, c.pet, c.ink_px, c.caret_px, c.icon_px, c.cells
                            );
                        }
                    }
                }
            }
        }
    }
}

/// A move-up scene's Enter, ms ([`Scene::MoveUp`]).
fn move_up_enter(scene: Scene) -> u64 {
    let (ev, _) = scene.events();
    ev.iter()
        .find(|(_, e)| matches!(e, crate::trail_letters_read::Ev::Key('\r', _)))
        .map_or(0, |(at, _)| *at)
}

/// THE COMPANION KEEPS OFF THE LETTERS A PROGRAM WRITES AND MOVES ITS CARET INTO (the trail
/// gallery's `move-up`, looks8 round 2): from the Enter on, no frame lays the companion's
/// drawn body on a lettered cell. The Enter hands the caret's empty row to `./k`, and the
/// cat watches it run from beside the line it ran instead of dropping onto that row, where
/// `[vim] NORMAL` was printed under its body a second later; the caret then moves up into a
/// block of lettered lines, and the cat perches on the blank row under the block, under the
/// caret's column, instead of far along the caret's row through the lines between. On the
/// reference dark with the cat and on Solarized Dark with the dog, the keys typed through the
/// console and, as the gallery types them (`aterm ctl key`), past it: there the cat reads
/// the Enter from its echo; and on the gallery's own frames, which a headless instance
/// advances its effects by; and with the Enter pressed before the cat has walked clear of
/// the line's last letters ([`Scene::MoveUpHasty`]), where it steps clear along the line
/// instead of dropping onto the program's row: judged from 120 ms after that Enter, as the
/// `k` the hand typed under its tail a moment before (the start of a typing run, not a
/// program's letter) is left behind. Negative control: without the law that keeps it
/// watching from beside the line ([`OldPath::WatchFromTheLine`] reverted alone) the cat lies
/// on `NORMAL`, each way, until the ink sends it off.
#[test]
fn the_companion_keeps_off_the_letters_a_program_writes() {
    for (scene, drive) in [
        (Scene::MoveUp, Drive::Console),
        (Scene::MoveUp, Drive::Ctl),
        (Scene::MoveUp, Drive::Gallery),
        (Scene::MoveUpHasty, Drive::Console),
        (Scene::MoveUpHasty, Drive::Ctl),
        (Scene::MoveUpHasty, Drive::Gallery),
    ] {
        let enter = move_up_enter(scene);
        assert!(enter > 0, "fixture: {scene:?} has its Enter");
        let enter = if scene == Scene::MoveUpHasty {
            enter + 120
        } else {
            enter
        };
        for (style, look) in [("rainbow kitty pet", DARK), ("rainbow dog", SOLARIZED_DARK)] {
            let s = sum(&coats_driven(style, look, scene, &[], drive), enter);
            assert!(
                s.frames > 20,
                "fixture: {style} on {} ({scene:?}, {drive:?}): {s:?}",
                look.name
            );
            assert_eq!(
                (s.on_ink, s.on_caret),
                (0, 0),
                "{style} on {} ({scene:?}, {drive:?}): the companion lay on letters or the \
                 caret after the Enter: {s:?}",
                look.name
            );
        }
        let old = sum(
            &coats_driven(
                "rainbow kitty pet",
                DARK,
                scene,
                &[OldPath::WatchFromTheLine],
                drive,
            ),
            enter,
        );
        assert!(
            old.on_ink >= 3,
            "negative control ({scene:?}, {drive:?}): dropped onto the program's row, the cat \
             lies on its letters: {old:?}"
        );
    }
}

/// The cat's sit after the hand's last key (`kitty_pet`'s `SIT_AFTER`), and the hop that
/// takes a cat waiting at the wall off the line's last letters then, and its landing.
const SAT_AND_HOPPED_MS: u64 = 1_400 + 600;

/// THE BURST NEVER LAYS A RUNNING OR RESTING COMPANION ON ITS LETTERS, ITS ICONS OR THE CARET
/// (the trail gallery's `burst`, looks8 rounds 2 and 3, r23 K6): no frame lays the
/// companion's drawn body on a lettered cell, the caret's cell or a word icon on glass (where
/// it lay on an icon, the icon yielded to it and came back: a blink), and no icon blinks, from
/// the first key to the last (the run's start, where round 2 caught the body on `echo the
/// qu` as the first keys were typed under the cat landing at the prompt, and the run to the
/// wall) and from the cat's sit on to the scene's end, 4.6 s past the last key. Between, the
/// cat waits at the wall for a wrap the hand may still type, the line's last letters over
/// it: no bound leaves for a wrap that has not come (r23 K6-1: leaving early, it hung over
/// the line and came down on the next line's start for a wrap that never came), and once the
/// hand has stopped it hops off them to the seat by the caret that covers none. It rests
/// there: its drawn box keeps its place, within its breath, through the scene's last second.
/// With the cat on dark and the dog on Solarized Dark, the keys typed through the console
/// and, as the gallery types them (`aterm ctl key`), past it. Negative control: without the
/// first keys' landing ahead of them ([`OldPath::FirstKeysAhead`]) they are typed under the
/// cat as it comes down at the prompt, with the writing's own law reverted as well
/// ([`OldPath::StillUnderWriting`], looks10 round 4): it runs a landing the writing reaches
/// off the keys too, so alone the old landing lies on them 7 frames, not 10.
#[test]
fn the_burst_never_lays_a_running_or_resting_companion_on_its_letters() {
    let (ev, tail) = Scene::Burst.events();
    let keys: Vec<u64> = ev
        .iter()
        .filter(|(_, e)| matches!(e, crate::trail_letters_read::Ev::Key(..)))
        .map(|(at, _)| *at)
        .collect();
    let (first_key, last_key) = (keys[0], keys[keys.len() - 1]);
    let end = last_key + tail;
    let rested = last_key + SAT_AND_HOPPED_MS;
    let on_letters = |judged: &[Judged], from: u64, to: u64| -> usize {
        judged
            .iter()
            .filter(|j| j.ms >= from && j.ms <= to && j.coat.ink_px > 0)
            .count()
    };
    for drive in [Drive::Console, Drive::Ctl] {
        for (style, look) in [("rainbow kitty pet", DARK), ("rainbow dog", SOLARIZED_DARK)] {
            let judged = coats_driven(style, look, Scene::Burst, &[], drive);
            assert!(
                judged.iter().any(|j| !j.coat.icons.is_empty())
                    && judged.last().is_some_and(|j| j.ms + 32 >= end),
                "fixture: {style} on {} ({drive:?}): the word pass seats its icons and the \
                 replay runs to the scene's end",
                look.name
            );
            let rest: Vec<(i32, i32, i32, i32)> = judged
                .iter()
                .filter(|j| j.ms + 1000 >= end)
                .filter_map(|j| j.coat.pet)
                .collect();
            let still = rest.len() > 50
                && rest.windows(2).all(|w| {
                    (w[1].0 - w[0].0).abs() <= 4
                        && (w[1].3 - w[0].3).abs() <= 2
                        && (w[1].0 - rest[0].0).abs() <= 6
                });
            assert!(
                still,
                "fixture: {style} on {} ({drive:?}): the companion has landed and rests by \
                 the scene's last second: {:?}",
                look.name,
                rest.first().zip(rest.last())
            );
            for (part, from, to) in [("run", first_key, last_key), ("rest", rested, end)] {
                let window: Vec<Judged> = judged
                    .iter()
                    .filter(|j| j.ms >= from && j.ms <= to)
                    .cloned()
                    .collect();
                let s = sum(&window, from);
                let icons = window.iter().filter(|j| j.coat.icon_px > 0).count();
                assert_eq!(
                    (s.on_ink, s.on_caret, icons, s.blinks),
                    (0, 0, 0, 0),
                    "{style} on {} ({drive:?}), its {part}: frames on letters, on the caret, \
                     on an icon, and icon blinks: {s:?}",
                    look.name
                );
            }
        }
        let old =
            |laws: &[OldPath]| coats_driven("rainbow kitty pet", DARK, Scene::Burst, laws, drive);
        let start = on_letters(
            &old(&[OldPath::FirstKeysAhead, OldPath::StillUnderWriting]),
            first_key,
            first_key + 700,
        );
        assert!(
            start >= 10,
            "negative control ({drive:?}): its landing kept where the caret was, the cat lies \
             on the first keys ({start} frames)"
        );
    }
}

/// THE SPLIT'S BURST NEVER LAYS A RUNNING OR RESTING COMPANION ON ITS LETTERS (the trail
/// gallery's `split`, looks8 round 3, r23 K6): the burst typed on the last row but one of the
/// caret's 6-row pane, so the row under the line is the pane's last, where the body's head
/// would lie in the line. From the first key to the last, and from the cat's sit on to the
/// scene's end, 4.6 s past the last key, no frame lays the companion's drawn body on a
/// lettered cell, the caret's cell or a word icon, no icon blinks, and it has come to rest by
/// the scene's last second: once the hand has stopped it hops off the line's last letters at
/// the wall onto the row over the line (round 3's first capture caught a bound coming down onto
/// `the` 1.65 s after the last key, to rest with its head in the line). With the cat on dark
/// and the dog on Solarized Dark, the keys typed through the console and, as the gallery types
/// them, past it.
#[test]
fn the_split_s_burst_never_lays_a_running_or_resting_companion_on_its_letters() {
    let scene = Scene::SplitBurst;
    let (ev, tail) = scene.events();
    let keys: Vec<u64> = ev
        .iter()
        .filter(|(_, e)| matches!(e, crate::trail_letters_read::Ev::Key(..)))
        .map(|(at, _)| *at)
        .collect();
    let (first_key, last_key) = (keys[0], keys[keys.len() - 1]);
    let end = last_key + tail;
    let rested = last_key + SAT_AND_HOPPED_MS;
    for drive in [Drive::Console, Drive::Ctl] {
        for (style, look) in [("rainbow kitty pet", DARK), ("rainbow dog", SOLARIZED_DARK)] {
            let judged = coats_driven(style, look, scene, &[], drive);
            let rest: Vec<(i32, i32, i32, i32)> = judged
                .iter()
                .filter(|j| j.ms + 1000 >= end)
                .filter_map(|j| j.coat.pet)
                .collect();
            let still = rest.len() > 50
                && rest.windows(2).all(|w| {
                    (w[1].0 - w[0].0).abs() <= 4
                        && (w[1].3 - w[0].3).abs() <= 2
                        && (w[1].0 - rest[0].0).abs() <= 6
                });
            assert!(
                still && judged.last().is_some_and(|j| j.ms + 32 >= end),
                "fixture: {style} on {} ({drive:?}): the replay runs to the scene's end and the \
                 companion rests by its last second: {:?}",
                look.name,
                rest.first().zip(rest.last())
            );
            for (part, from, to) in [("run", first_key, last_key), ("rest", rested, end)] {
                let window: Vec<Judged> = judged
                    .iter()
                    .filter(|j| j.ms >= from && j.ms <= to)
                    .cloned()
                    .collect();
                let s = sum(&window, from);
                let icons = window.iter().filter(|j| j.coat.icon_px > 0).count();
                assert_eq!(
                    (s.on_ink, s.on_caret, icons, s.blinks),
                    (0, 0, 0, 0),
                    "{style} on {} ({drive:?}), its {part}: frames on letters, on the caret, \
                     on an icon, and icon blinks: {s:?}",
                    look.name
                );
            }
        }
    }
}

/// The prompt between lettered rows: `above` on row 3, the prompt `$ ` on row 4 and `below`
/// on row 5 from the start, the companion coming up beside it, and the typing scene's line
/// typed at 60 ms a key from 1 s, keys through the console, every frame's coat read.
fn typed_between_lettered_rows(
    style: &str,
    look: Look,
    (above, below): (&str, &str),
    old: &[OldPath],
) -> Vec<Judged> {
    typed_under_a_lettered_row(style, look, 4, (above, below), old, false).0
}

/// The prompt `$ ` on row `prompt` (0-based) under `above`, with `below` on the row under it
/// where there is one, the companion coming up beside it, and the typing scene's line typed at
/// 60 ms a key from 1 s, keys through the console, every frame's coat read, its letters judged
/// against their own readings where `letters` says ([`Replay::own`]); and the replay's cell
/// height.
fn typed_under_a_lettered_row(
    style: &str,
    look: Look,
    prompt: u16,
    (above, below): (&str, &str),
    old: &[OldPath],
    letters: bool,
) -> (Vec<Judged>, i32) {
    let mut r = Replay::new(style, look, TrailColor::Automatic);
    r.coat = true;
    r.letters = letters;
    r.own = letters;
    r.fx.set_sparkle_enabled(true);
    for &law in old {
        r.fx.set_pet_old_path(law, true);
    }
    let ch = r.cell.1 as i32;
    let under = if prompt + 1 < crate::trail_letters_read::ROWS {
        format!("\x1b[{}H{below}", prompt + 2)
    } else {
        String::new()
    };
    let page = format!("\x1b[2J\x1b[{prompt}H{above}{under}\x1b[{}H$ ", prompt + 1);
    let mut ev = vec![(0, Ev::Out(page.into_bytes()))];
    ev.extend(typed(TYPING_TEXT, TYPING_FROM, 60));
    (r.run(&ev, 1_600), ch)
}

/// The letters of row `row` judged from `from` ms that read under their bar, the lower of
/// [`crate::trail_letters_read::BAR`] and the share of their own reading a dim letter keeps under
/// the trail's lights ([`crate::trail_letters_read::OWN_SHARE`], the letter guard's), and the
/// worst of them with its own reading. A body laid on the letters takes them far under it
/// (2.00:1 against their own 4.51, review9fix round 3).
fn letters_under_their_bar(judged: &[Judged], from: u64, row: usize) -> (usize, f64, f64) {
    let mut under = 0;
    let (mut worst, mut own) = (f64::INFINITY, f64::INFINITY);
    for l in judged
        .iter()
        .filter(|j| j.ms >= from)
        .flat_map(|j| j.letters().iter().filter(|l| l.row == row))
    {
        let bar = crate::trail_letters_read::BAR.min(crate::trail_letters_read::OWN_SHARE * l.own);
        if l.judged() < bar - 1e-6 {
            under += 1;
        }
        if l.judged() < worst {
            (worst, own) = (l.judged(), l.own);
        }
    }
    (under, worst, own)
}

/// The right edge, frame px, of the last letter of a row the replay writes from its column 0
/// (`Replay::new`'s cells, `cw` px across, from the frame's left edge).
fn line_end(row: &str) -> i32 {
    let cw = Replay::new("off", DARK, TrailColor::Automatic).cell.0 as i32;
    cw * row.trim_end().chars().count() as i32
}

/// The frames from `from` ms, once the head law first holds the body in row `row` (cell height
/// `ch`: its box's top at or under the row's top edge while it stands on the row, its feet in
/// the row's lower half or drawn down into the row under), in which the box rises past that
/// edge again while it stands there: a held body let go and drawn whole, to be drawn down
/// again at the next word. A box wholly past `line_end` (frame px: the row above's last
/// letter's right edge) is past the line's end, where the head law lets the body go whole
/// (`OldPath::LineEndIsNoGap`), and is no rise.
fn rises_past_the_row(judged: &[Judged], from: u64, row: i32, ch: i32, line_end: i32) -> usize {
    let (top, feet) = (row * ch, (row * ch + ch / 2)..((row + 2) * ch));
    let boxes: Vec<(i32, i32)> = judged
        .iter()
        .filter(|j| j.ms >= from)
        .filter_map(|j| j.coat.pet)
        .filter(|&(_, _, _, y1)| feet.contains(&y1))
        .map(|(x0, _, y0, _)| (x0, y0))
        .collect();
    boxes
        .iter()
        .position(|&(_, y0)| y0 >= top)
        .map_or(0, |held| {
            boxes[held..]
                .iter()
                .filter(|&&(x0, y0)| y0 < top - ch / 4 && x0 < line_end)
                .count()
        })
}

/// Frames from `from` ms whose drawn box meets a cell holding a glyph in a row of `rows`, by a
/// pixel or more, as `aterm-lab trail-companion` reads a gallery frame ([`Coat::box_cells`]).
fn boxes_on_glyphs(judged: &[Judged], from: u64, rows: RangeInclusive<usize>) -> usize {
    judged
        .iter()
        .filter(|j| j.ms >= from && j.coat.box_cells.iter().any(|c| rows.contains(&c.0)))
        .count()
}

/// How long (ms) after the first key of a run typed from rest its letters may pass over the
/// companion getting going on foot beside the prompt: no flight leaves at a typing run's start
/// (r24 K7-1: looks8's first keys' spring hopped the cat ahead there, after almost every pause
/// in a person's typing, the owner's "jumping"), so the first keys at 60 ms run over its tail
/// until its walk becomes a run; drawn under the text, the companion has each letter knocked
/// out of its body ([`crate::companion_knock_out`]). The effects' own `FIRST_KEYS_LAG_MS`.
const FIRST_KEYS_LAG_MS: u64 = 300;

/// `judged` less the frames in which the first letters of a run typed from rest on row `row`,
/// its first key at `first_key` ms, pass over the companion getting going
/// ([`FIRST_KEYS_LAG_MS`]): those within that time of the first key whose box and body meet
/// that row's letters and no other row's, nor the caret's cell.
fn past_the_first_keys(judged: &[Judged], first_key: u64, row: usize) -> Vec<Judged> {
    judged
        .iter()
        .filter(|j| {
            let c = &j.coat;
            !((first_key..first_key + FIRST_KEYS_LAG_MS).contains(&j.ms)
                && c.caret_px == 0
                && c.box_cells.iter().all(|b| b.0 == row)
                && c.cells.iter().all(|b| b.0 == row))
        })
        .cloned()
        .collect()
}

/// When the typing scenes ([`typed_under_a_lettered_row`], [`steamed_between_lettered_rows`])
/// type their first key, ms.
const TYPING_FROM: u64 = 1_000;

/// THE COMPANION'S WHOLE BOX KEEPS OFF THE LETTERS, THE ROW ABOVE ITS FEET AS WELL AS ITS
/// OWN (looks9's review): its drawn box stands 1.7 rows tall, so a cat on the prompt's row
/// reaches 0.7 of a row into the row above; with a command's output on that row (the usual
/// case: any output, then a prompt) its ears and back lay on the letters' descenders for
/// most of the typing (`gjpqy` read 1.6:1 under the dog). It is drawn down its own row
/// instead, its head in the prompt's row past the caret and its feet in the blank row under
/// it ([`aterm_effects::kitty_pet::head_law`]). From 600 ms (come to the prompt) to the
/// scene's end, no frame's drawn box meets a cell of the row above holding a glyph, by a pixel
/// or more (the gallery's `trail-companion` read), or lays the body on a letter's ink or the
/// caret's cell, but for the first keys' letters on the prompt's row as the cat gets going on
/// foot ([`past_the_first_keys`]): with a row of descenders and with a sentence above the
/// prompt, the cat on dark and the dog on Solarized Dark. Negative control: without the head
/// law ([`OldPath::HeadUnderLetters`] reverted alone) the box lies on the row above's letters
/// (186 frames under `gjpqy`, 73 under the sentence, measured by ink).
///
/// Under ragged output above (words six cells apart, a long listing's columns, words 24 cells
/// apart), the gaps wider than the body: the law reads the row above as far past the body,
/// either side, as its ease can take it ([`OldPath::HeadLooksAhead`]; reverted alone, the box
/// of the cat getting going out of the gap it rests in meets the next word's first letters),
/// at the speed the run can reach in the ease and past a stand the writing carries on
/// ([`OldPath::LookAheadAtTopSpeed`]; reverted alone, under words 24 cells apart the cat
/// speeding up off the first keys meets `efgh` before its sink is done, r24 K7-1), and once
/// drawn down the body stays down across a gap wider than that ([`OldPath::HeadHeldAcrossGaps`];
/// reverted alone, under words 24 cells apart it rises whole in the gap, its box past the
/// prompt row's top, and is drawn down again at the next word). Review9fix round 3's review:
/// 13 of 211 frames on the row above's letters, at its columns 10-12, 20 and 30-32.
#[test]
fn the_companion_keeps_off_a_lettered_row_above_the_prompt() {
    let gj = "gjpqy ".repeat(12);
    let fox = "the quick brown fox jumps over the lazy dog";
    let ragged = "abcd      efgh      ijkl      mnop      qrst      uvwx      yz";
    let listing = "ls -la      total 48      drwxr-xr-x      staff      Oct 29";
    let wide = format!("abcd{0}efgh{0}ijkl", " ".repeat(24));
    let from = 600;
    for rows in [
        (gj.as_str(), ""),
        (fox, ""),
        (ragged, ""),
        (listing, ""),
        (wide.as_str(), ""),
    ] {
        for (style, look) in [
            ("rainbow kitty pet", DARK),
            ("rainbow kitty underline", DARK),
            ("rainbow dog", SOLARIZED_DARK),
        ] {
            let (judged, ch) = typed_under_a_lettered_row(style, look, 4, rows, &[], false);
            let s = sum(&past_the_first_keys(&judged, TYPING_FROM, 4), from);
            let boxed = boxes_on_glyphs(&judged, from, 3..=3);
            let rises = rises_past_the_row(&judged, from, 4, ch, line_end(rows.0));
            assert_eq!(
                (boxed, s.on_ink, s.on_caret, rises),
                (0, 0, 0, 0),
                "{style} on {} between {rows:?}: frames whose box meets a glyph's cell in the row \
                 above, whose body lies on ink, on the caret, whose box rises past the row once \
                 held: {s:?}",
                look.name
            );
        }
        let old = typed_between_lettered_rows(
            "rainbow kitty pet",
            DARK,
            rows,
            &[OldPath::HeadUnderLetters],
        );
        let old_boxed = boxes_on_glyphs(&old, from, 3..=3);
        assert!(
            old_boxed >= 10,
            "negative control between {rows:?}: its head among the row above's letters in \
             {old_boxed} frames: {:?}",
            sum(&old, from)
        );
    }
    let (held, ch) = typed_under_a_lettered_row(
        "rainbow kitty pet",
        DARK,
        4,
        (&wide, ""),
        &[OldPath::HeadHeldAcrossGaps],
        false,
    );
    assert!(
        rises_past_the_row(&held, from, 4, ch, line_end(&wide)) > 0,
        "negative control (the hold): the box never rose past the row once held"
    );
    let (late, _) = typed_under_a_lettered_row(
        "rainbow kitty pet",
        DARK,
        4,
        (ragged, ""),
        &[OldPath::HeadLooksAhead],
        false,
    );
    assert!(
        boxes_on_glyphs(&late, from, 3..=3) > 0,
        "negative control (the look ahead): the box never met the next word's letters: {:?}",
        sum(&late, from)
    );
    let (short, _) = typed_under_a_lettered_row(
        "rainbow kitty pet",
        DARK,
        4,
        (&wide, ""),
        &[OldPath::LookAheadAtTopSpeed],
        false,
    );
    assert!(
        boxes_on_glyphs(&short, from, 3..=3) > 0,
        "negative control (the look ahead at the run's top speed): the box never met the next \
         word's letters: {:?}",
        sum(&short, from)
    );
}

/// THE COMPANION'S BOX KEEPS OFF A LETTERED ROW ABOVE A PROMPT ON THE GRID'S LAST ROW (review9fix
/// round 3's review): once a screen fills every prompt sits on the last row, with no row under
/// it to draw the body down into, and the head law left that row out: the box lay on the
/// output's letters directly above in every frame (211 of 211), and on Solarized Dark the
/// dog's body took them to 2.00:1 against their own 4.51. The body is fitted to its own row
/// there ([`aterm_effects::kitty_pet::head_law`], the row under read as inked). From 600 ms to
/// the scene's end, no frame's box meets a glyph's cell in the row above, lays the body on ink
/// or the caret's cell, and no letter of that row reads under the lower of the bar and the share
/// of its own reading the trail's lights leave a dim letter (the letter guard's): under a row of
/// descenders and under a sentence, every companion style on dark and
/// the dog on Solarized Dark and GitHub Light. Negative control ([`OldPath::RowFit`] reverted
/// alone, the dog on Solarized Dark): its box meets the row above's letters.
#[test]
fn the_companion_keeps_off_a_lettered_row_above_a_prompt_on_the_last_row() {
    let gj = "gjpqy ".repeat(12);
    let fox = "the quick brown fox jumps over the lazy dog";
    let (from, last) = (600, crate::trail_letters_read::ROWS - 1);
    let above = usize::from(last - 1);
    let mut runs: Vec<(&str, Look)> = COMPANIONS.iter().map(|&s| (s, DARK)).collect();
    runs.extend([
        ("rainbow dog", SOLARIZED_DARK),
        ("rainbow dog", GITHUB_LIGHT),
    ]);
    for rows in [(gj.as_str(), ""), (fox, "")] {
        for &(style, look) in &runs {
            let (judged, _) = typed_under_a_lettered_row(style, look, last, rows, &[], true);
            let s = sum(&judged, from);
            let boxed = boxes_on_glyphs(&judged, from, above..=above);
            let (under, worst, own) = letters_under_their_bar(&judged, from, above);
            assert!(
                s.frames >= 40,
                "fixture: {style} on {} under {rows:?} is replayed ({} frames)",
                look.name,
                s.frames
            );
            assert_eq!(
                (boxed, s.on_ink, s.on_caret, under),
                (0, 0, 0, 0),
                "{style} on {} under {rows:?} on the last row: frames whose box meets a glyph's \
                 cell in the row above, whose body lies on ink, on the caret, and letters of the \
                 row above under their bar (worst {worst:.2} against its own {own:.2}): {s:?}",
                look.name
            );
        }
    }
    let (old, _) = typed_under_a_lettered_row(
        "rainbow dog",
        SOLARIZED_DARK,
        last,
        (&gj, ""),
        &[OldPath::RowFit],
        false,
    );
    let old_boxed = boxes_on_glyphs(&old, from, above..=above);
    assert!(
        old_boxed >= 10,
        "negative control: its box among the row above's letters in {old_boxed} frames: {:?}",
        sum(&old, from)
    );
}

/// THE COMPANION KEEPS OFF THE ECHO OVER A RETURN AT THE GRID'S LAST ROW (review9's gallery,
/// `enter-bottom`): the line typed on the last row and entered scrolls up with its output
/// under it, and the cat bounds back along the last row to the new prompt's caret under the
/// echo and the output. The bound keeps its head under the lettered row two above its feet
/// (the head law in flight, `HEAD_CLEAR`): reaching a quarter row into it, its ears met the
/// echo's `n fox` mid-bound in the gallery's capture. The new prompt on the last row has the
/// command's output directly above it, and where the body stands under that output it is
/// fitted to its own row (the head law on the grid's last row: the row under read as inked).
/// From the Return to 2.5 s after it, no frame's drawn box meets a cell holding a glyph on any
/// row above the last (the echo's, the numbers' and the output line's directly above), by a
/// pixel or more, for every companion style on dark, Solarized Dark and both light themes: with
/// the scene's output, with a blank line printed before the new prompt (the row above the
/// bound blank and the output's line two above it), and with an output line that runs on to
/// within two cells of the row's end, so no seat past it fits a body and the cat stands under
/// it: the long line lands over the cat resting whole beside the caret's line, and the head
/// law judges a body as the page under it changes ([`OldPath::JudgedAsThePageChanges`]).
/// Negative controls, each reverted alone: [`OldPath::RowFit`], under the long line its box
/// meets the line above the last row; [`OldPath::JudgedAsThePageChanges`], the long line lies
/// under its whole box until it next moves (31 frames, half a second, for every companion
/// style); [`OldPath::HeadClearInFlight`], with the blank line, its
/// box meets the output's letters two rows above mid-bound (in the scene as the gallery has it,
/// the fit now keeps the bound in its row too); [`OldPath::ArcJudgedInTheAir`], with the blank
/// line, the cat's box meets them as the output lands after the bound's take-off (18 frames).
#[test]
fn the_companion_keeps_off_the_echo_over_a_return_at_the_last_row() {
    let enter = 1_000 + 60 * TYPING_TEXT.len() as u64 + 200;
    let long = format!("{:.70}\r\n$ ", "the quick brown fox ".repeat(4));
    let outputs: [&[u8]; 3] = [
        b"the quick brown fox\r\n$ ",
        b"the quick brown fox\r\n\r\n$ ",
        long.as_bytes(),
    ];
    let run = |style: &str, look: Look, old: &[OldPath], output: &[u8]| {
        let mut r = Replay::new(style, look, TrailColor::Automatic);
        r.coat = true;
        r.fx.set_sparkle_enabled(true);
        for &law in old {
            r.fx.set_pet_old_path(law, true);
        }
        let (mut ev, _) = Scene::EnterBottom.events();
        let last = ev.last_mut().expect("fixture: the scene's events");
        assert!(
            matches!(&last.1, Ev::Out(b) if b.as_slice() == outputs[0]),
            "fixture: the scene ends on its output and the new prompt"
        );
        last.1 = Ev::Out(output.to_vec());
        r.run(&ev, 2_500)
    };
    for look in [DARK, SOLARIZED_DARK, DEFAULT_LIGHT, GITHUB_LIGHT] {
        for style in COMPANIONS {
            for output in outputs {
                let boxed = boxes_on_glyphs(&run(style, look, &[], output), enter, 0..=12);
                assert_eq!(
                    boxed,
                    0,
                    "{style} on {} (output {:?}): frames whose box meets a glyph's cell above \
                     its feet",
                    look.name,
                    String::from_utf8_lossy(output)
                );
            }
        }
    }
    let old = run(
        "flat rainbow",
        DARK,
        &[OldPath::HeadClearInFlight],
        outputs[1],
    );
    assert!(
        boxes_on_glyphs(&old, enter, 0..=11) > 0,
        "negative control: the bound's box never met the output's letters two rows above"
    );
    let old = run(
        "rainbow kitty pet",
        DARK,
        &[OldPath::ArcJudgedInTheAir],
        outputs[1],
    );
    assert!(
        boxes_on_glyphs(&old, enter, 0..=11) > 0,
        "negative control: judged at take-off alone, the bound's box never met the output \
         landed two rows above"
    );
    let old = run("flat rainbow", DARK, &[OldPath::RowFit], outputs[2]);
    assert!(
        boxes_on_glyphs(&old, enter, 12..=12) > 0,
        "negative control: the box never met the long line above the last row"
    );
    let old = run(
        "rainbow kitty pet",
        DARK,
        &[OldPath::JudgedAsThePageChanges],
        outputs[2],
    );
    assert!(
        boxes_on_glyphs(&old, enter, 12..=12) >= 10,
        "negative control: judged only as it moves, the box never lay under the long line \
         printed over it"
    );
}

/// Every run a sweep over the steam scene's rows reads: each of [`STEAM_ROWS`], every companion
/// style on dark, Solarized Dark and both light themes, its keys driven both ways.
fn steam_runs() -> Vec<((&'static str, &'static str), Look, &'static str, Drive)> {
    let mut runs = Vec::new();
    for rows in STEAM_ROWS {
        for look in [DARK, SOLARIZED_DARK, DEFAULT_LIGHT, GITHUB_LIGHT] {
            for style in COMPANIONS {
                for drive in [Drive::Ctl, Drive::Console] {
                    runs.push((rows, look, style, drive));
                }
            }
        }
    }
    runs
}

/// The gallery's `steam` scene in the replay on `page` ([`steam_page`]), the typing scene's
/// line typed at 60 ms a key from 1 s, then 300 ms on, twelve Backspaces 50 ms apart, keys
/// driven as `drive` says, every frame's coat read to 1.6 s after the last; and the replay's
/// cell height.
fn steamed_between_lettered_rows(
    style: &str,
    look: Look,
    page: &str,
    old: &[OldPath],
    drive: Drive,
) -> (Vec<Judged>, i32) {
    let mut r = Replay::new(style, look, TrailColor::Automatic);
    r.coat = true;
    r.keys_unnoted = drive != Drive::Console;
    r.fx.set_sparkle_enabled(true);
    for &law in old {
        r.fx.set_pet_old_path(law, true);
    }
    let ch = r.cell.1 as i32;
    let mut ev = vec![(0, Ev::Out(page.as_bytes().to_vec()))];
    ev.extend(typed(TYPING_TEXT, TYPING_FROM, 60));
    let erase_from = TYPING_FROM + 60 * TYPING_TEXT.len() as u64 + 300;
    ev.extend((0..12u64).map(|k| {
        (
            erase_from + 50 * k,
            Ev::Key('\u{7f}', b"\x08 \x08".to_vec()),
        )
    }));
    (r.run(&ev, 1_600), ch)
}

/// The rows under the prompt the companion's seats are sought on (the effects' `HEAD_SEAT_ROWS`).
const SEAT_ROWS: usize = 3;

/// Where the `steam` scene's rows lie ([`steam_page`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Steam {
    /// As the gallery has them: the rows under the row under blank.
    Gallery,
    /// No seat within the companion's reach draws its body whole, so the head law's fit
    /// governs ([`OldPath::WholeSeatOverFit`] seats the body whole wherever such a seat lies
    /// within reach): the row above written on to the margin, and the row under, written on to
    /// the margin, on each of the [`SEAT_ROWS`] rows under the prompt.
    Deep,
    /// As the gallery has them on the alternate screen, where a full-screen program's rows
    /// under its line are its own and the cat keeps its line's row, fitted there.
    FullScreen,
}

/// The `steam` scene's page: `above` on row 3, the prompt `$ ` on row 4 and `below` on row 5,
/// laid as `steam` says.
fn steam_page((above, below): (&str, &str), steam: Steam) -> String {
    let cols = usize::from(crate::trail_letters_read::COLS);
    let to_margin = |t: &str| {
        format!("{t} ")
            .repeat(cols)
            .chars()
            .take(cols)
            .collect::<String>()
    };
    match steam {
        Steam::Gallery => format!("\x1b[2J\x1b[4H{above}\x1b[6H{below}\x1b[5H$ "),
        Steam::FullScreen => {
            format!("\x1b[?1049h\x1b[2J\x1b[4H{above}\x1b[6H{below}\x1b[5H$ ")
        }
        Steam::Deep => {
            let mut page = format!("\x1b[2J\x1b[4H{}", to_margin(above));
            for k in 0..SEAT_ROWS {
                page.push_str(&format!("\x1b[{}H{}", 6 + k, to_margin(below)));
            }
            page.push_str("\x1b[5H$ ");
            page
        }
    }
}

/// The frames from `from` ms in which the companion is on row `row` (cell height `ch`): its
/// box's foot in the lower half of the row, or a pixel under it. A flight to or from another
/// row is judged by the flight's laws, not the head law's: its foot leaves the row, and a hop
/// along the row keeps it there (the head law caps a fitted body's lift at 0.08 of a row).
fn on_row(judged: &[Judged], from: u64, row: i32, ch: i32) -> Vec<Judged> {
    let band = (row * ch + ch / 2)..=((row + 1) * ch + 1);
    judged
        .iter()
        .filter(|j| j.ms >= from && j.coat.pet.is_some_and(|(_, _, _, y1)| band.contains(&y1)))
        .cloned()
        .collect()
}

/// The steam scene's rows: the gallery's own, and a row of descenders over a sentence.
const STEAM_ROWS: [(&str, &str); 2] = [
    (
        "Above2: The lazy dog jogged by, every glyph legible.",
        "Below: pqgyj Wide test {braces} [ok]",
    ),
    (
        "gjpqy gjpqy gjpqy gjpqy gjpqy gjpqy gjpqy gjpqy gjpqy gjpqy",
        "the quick brown fox jumps over the lazy dog, every glyph legible",
    ),
];

/// THE COMPANION'S WHOLE BOX KEEPS OFF LETTERED ROWS ABOVE AND UNDER THE PROMPT (review9fix
/// round 1's `steam`): with a lettered row under the prompt too, and no blank row under it in
/// the companion's reach to take the body down ([`steam_page`]'s deep page: the rows above
/// and under lettered to the margin), on its row its head lay on the row above's letters (107
/// of 112 of the gallery's `steam` frames). It is fitted to its own row instead, whole and
/// smaller, its feet where they stand ([`aterm_effects::kitty_pet::head_law`]). From 600 ms
/// (come to the prompt) to the scene's end, through the line's typing and twelve Backspaces,
/// no frame's drawn box meets a cell holding a glyph on any row, by a pixel or more (the
/// gallery's `trail-companion` read), or lays the body on a letter's ink or the caret's cell:
/// every companion style on dark, Solarized Dark and both light themes, between the gallery's
/// own rows and between descenders and a sentence, its keys typed at the window (the
/// console's) and as the gallery types them (`aterm ctl key`). Every frame the body is on the
/// prompt's row is judged ([`on_row`]), and every frame, flights included, keeps its box off
/// the row above's letters. Negative control ([`OldPath::RowFit`] reverted alone, the cat on
/// dark, `ctl` keys): its box meets the row above's letters.
#[test]
fn the_companion_keeps_off_lettered_rows_above_and_under_the_prompt() {
    let from = 600;
    aterm_spec::check::each(&steam_runs(), |&(rows, look, style, drive)| {
        let page = steam_page(rows, Steam::Deep);
        let (judged, ch) = steamed_between_lettered_rows(style, look, &page, &[], drive);
        let above = boxes_on_glyphs(&judged, from, 3..=3);
        let judged = past_the_first_keys(&on_row(&judged, from, 4, ch), TYPING_FROM, 4);
        let s = sum(&judged, from);
        let boxed = boxes_on_glyphs(&judged, from, 0..=23);
        assert!(
            s.frames >= 40,
            "fixture: {style} on {} between {rows:?} ({drive:?}) is on the prompt's row ({} \
             frames)",
            look.name,
            s.frames
        );
        assert_eq!(
            (boxed, s.on_ink, s.on_caret, above),
            (0, 0, 0, 0),
            "{style} on {} between {rows:?} ({drive:?}): frames on the prompt's row whose box \
             meets a glyph's cell, whose body lies on ink, on the caret; frames anywhere whose \
             box meets the row above's letters: {s:?}",
            look.name
        );
    });
    aterm_spec::check::each(&STEAM_ROWS, |&rows| {
        let page = steam_page(rows, Steam::Deep);
        let (old, ch) = steamed_between_lettered_rows(
            "rainbow kitty pet",
            DARK,
            &page,
            &[OldPath::RowFit],
            Drive::Ctl,
        );
        let old = on_row(&old, from, 4, ch);
        let old_boxed = boxes_on_glyphs(&old, from, 3..=3);
        assert!(
            old_boxed >= 10,
            "negative control between {rows:?}: its head among the row above's letters in \
             {old_boxed} frames: {:?}",
            sum(&old, from)
        );
    });
}

/// THE WHOLE BODY BEFORE THE FIT (composed r1's `steam`, its judges' T2; O2(c), O3;
/// [`OldPath::WholeSeatOverFit`]): between the gallery's two lettered rows the head law fitted
/// the companion to the prompt's row, a token at 0.54 of its height beside the caret for the
/// whole line's typing and its Backspaces, where the rows under the row under were blank. A
/// stand the head law would fit now gives way to the nearest seat within the companion's reach
/// where its body is whole, and it is fitted only where no such seat lies within reach
/// ([`Steam::Deep`], read by the tests above). On the gallery's page ([`Steam::Gallery`]),
/// between its own rows and between descenders and a sentence, from 600 ms to the scene's end:
/// no frame draws the box fitted (no taller than 1.05 rows), none meets a glyph's cell on any
/// row by a pixel or more, and none lays the body on ink or the caret's cell, for every
/// companion style on dark, Solarized Dark and both light themes, its keys typed at the window
/// and as the gallery types them. In `context` and the last-row leap the seat past a line's end
/// already seats the body whole, and the law moves no frame there
/// ([`the_companion_is_drawn_whole_past_the_row_above_s_last_letter`],
/// [`a_seat_past_a_line_s_end_keeps_the_body_whole`]). Negative control (the law reverted
/// alone, the cat on dark, `ctl` keys, the gallery's own rows): it is fitted beside the caret.
#[test]
fn a_whole_seat_in_reach_comes_before_the_fit() {
    let from = 600;
    let heights = |judged: &[Judged], ch: i32| -> Vec<f64> {
        judged
            .iter()
            .filter(|j| j.ms >= from)
            .filter_map(|j| j.coat.pet)
            .map(|(_, _, y0, y1)| f64::from(y1 - y0) / f64::from(ch))
            .collect()
    };
    aterm_spec::check::each(&steam_runs(), |&(rows, look, style, drive)| {
        let page = steam_page(rows, Steam::Gallery);
        let (judged, ch) = steamed_between_lettered_rows(style, look, &page, &[], drive);
        let h = heights(&judged, ch);
        let s = sum(&judged, from);
        assert!(
            h.len() >= 200,
            "fixture: {style} on {} between {rows:?} ({drive:?}) is drawn ({} frames)",
            look.name,
            h.len()
        );
        let fitted = h.iter().filter(|&&h| h <= 1.05).count();
        let boxed = boxes_on_glyphs(&judged, from, 0..=23);
        assert_eq!(
            (fitted, boxed, s.on_ink, s.on_caret),
            (0, 0, 0, 0),
            "{style} on {} between {rows:?} ({drive:?}): frames fitted, whose box meets a \
             glyph's cell, whose body lies on ink, on the caret: {s:?}",
            look.name
        );
    });
    let page = steam_page(STEAM_ROWS[0], Steam::Gallery);
    let (old, ch) = steamed_between_lettered_rows(
        "rainbow kitty pet",
        DARK,
        &page,
        &[OldPath::WholeSeatOverFit],
        Drive::Ctl,
    );
    let fitted = heights(&old, ch).iter().filter(|&&h| h <= 1.05).count();
    assert!(
        fitted >= 40,
        "negative control: fitted beside the caret in {fitted} frames"
    );
}

/// A BODY FITTED TO ITS ROW STAYS FITTED ALONG IT ([`OldPath::FitHeldAlongTheRow`]): between a
/// long lettered row above and a row under whose letters end a dozen cells in, the cat is
/// fitted to its row where the row under holds ink, and the line's typing runs it on past
/// those letters, where a blank stretch of the row under could take it down. Drawn down into it
/// whole and fitted again at the next letters, it grew and shrank as it went (the gallery's
/// `steam` capture: the box drawn down at `bs03` and `bs06` and back up at `bs09`, review9fix
/// round 1's judge). The law governs where no whole seat lies in reach: on the normal screen
/// the blank rows under seat the body whole ([`OldPath::WholeSeatOverFit`]), so it is read on
/// the alternate screen ([`Steam::FullScreen`]), where the cat keeps its line's row. From its
/// first fitted frame on, no frame of the cat on dark or the dog on Solarized Dark draws the box
/// whole (taller than 1.4 rows) while the caret keeps its row, and none meets a lettered cell.
/// Negative control (the law reverted alone, the cat on dark): its box is drawn whole again
/// past the row under's letters.
#[test]
fn a_body_fitted_to_its_row_stays_fitted_along_it() {
    let above = "the quick brown fox jumps over the lazy dog, every glyph legible here";
    let below = "Below: pqgyj";
    let whole_after_fit = |style: &str, look: Look, old: &[OldPath]| -> (usize, usize, usize) {
        let mut r = Replay::new(style, look, TrailColor::Automatic);
        r.coat = true;
        r.fx.set_sparkle_enabled(true);
        for &law in old {
            r.fx.set_pet_old_path(law, true);
        }
        let ch = r.cell.1 as f64;
        let page = steam_page((above, below), Steam::FullScreen);
        let mut ev = vec![(0, Ev::Out(page.into_bytes()))];
        ev.extend(typed(TYPING_TEXT, TYPING_FROM, 60));
        let judged = r.run(&ev, 1_600);
        let heights: Vec<f64> = judged
            .iter()
            .filter(|j| j.ms >= 600)
            .filter_map(|j| j.coat.pet)
            .map(|(_, _, y0, y1)| f64::from(y1 - y0) / ch)
            .collect();
        let first_fit = heights.iter().position(|&h| h <= 1.05);
        let whole = first_fit.map_or(0, |at| heights[at..].iter().filter(|&&h| h > 1.4).count());
        (
            usize::from(first_fit.is_some()),
            whole,
            boxes_on_glyphs(&past_the_first_keys(&judged, TYPING_FROM, 4), 600, 0..=23),
        )
    };
    for (style, look) in [("rainbow kitty pet", DARK), ("rainbow dog", SOLARIZED_DARK)] {
        let (fitted, whole, boxed) = whole_after_fit(style, look, &[]);
        assert_eq!(
            (fitted, whole, boxed),
            (1, 0, 0),
            "{style} on {}: fitted at all, frames drawn whole after its first fitted frame, \
             frames whose box meets a lettered cell",
            look.name
        );
    }
    let (fitted, whole, _) =
        whole_after_fit("rainbow kitty pet", DARK, &[OldPath::FitHeldAlongTheRow]);
    assert!(
        fitted == 1 && whole > 0,
        "negative control: never drawn whole after its first fitted frame ({whole} frames)"
    );
}

/// A BODY HELD IN ITS ROW IS DRAWN NO TALLER THAN ITS FIT ([`OldPath::HeldNoTaller`]): in the
/// `steam` scene typed at the window, a cat fitted to the prompt's row between two lettered rows
/// has 0.08 of a row over its ears, and a pose's stretch (a purr's swell, an ear's twitch up)
/// took them 3 px into the row above's letters. With the law no frame of the cat on dark or the
/// dog on Solarized Dark on the prompt's row ([`on_row`]) meets a lettered cell, where no seat
/// in reach draws the body whole ([`Steam::Deep`]) and on the alternate screen
/// ([`Steam::FullScreen`]). Negative control (the law reverted alone, the cat on dark, on the
/// alternate screen): a frame's box meets the row above's letters.
#[test]
fn a_body_held_in_its_row_is_drawn_no_taller_than_its_fit() {
    let from = 600;
    let run = |style: &str, look: Look, steam: Steam, old: &[OldPath]| {
        let page = steam_page(STEAM_ROWS[0], steam);
        let (judged, ch) = steamed_between_lettered_rows(style, look, &page, old, Drive::Console);
        let on = past_the_first_keys(&on_row(&judged, from, 4, ch), TYPING_FROM, 4);
        assert!(
            on.len() >= 40,
            "fixture: {style} on {} ({steam:?}) rests on the prompt's row",
            look.name
        );
        boxes_on_glyphs(&on, from, 0..=23)
    };
    for steam in [Steam::Deep, Steam::FullScreen] {
        for (style, look) in [("rainbow kitty pet", DARK), ("rainbow dog", SOLARIZED_DARK)] {
            assert_eq!(
                run(style, look, steam, &[]),
                0,
                "{style} on {} ({steam:?}): frames on the prompt's row whose box meets a \
                 lettered cell",
                look.name
            );
        }
    }
    assert!(
        run(
            "rainbow kitty pet",
            DARK,
            Steam::FullScreen,
            &[OldPath::HeldNoTaller]
        ) > 0,
        "negative control: stretched past its fit, the box never met the row above's letters"
    );
}

/// The gallery's `context` in the replay ([`Scene::Context`]), the companion's laws in `old`
/// reverted, keys through the console, every frame's coat read; and the replay's cell height.
fn context_coats(style: &str, look: Look, old: &[OldPath]) -> (Vec<Judged>, i32) {
    let mut r = Replay::new(style, look, TrailColor::Automatic);
    r.coat = true;
    r.fx.set_sparkle_enabled(true);
    for &law in old {
        r.fx.set_pet_old_path(law, true);
    }
    let ch = r.cell.1 as i32;
    let (ev, tail) = Scene::Context.events();
    (r.run(&ev, tail), ch)
}

/// The drawn box's heights, in rows, over the scene's last `ms` milliseconds.
fn last_heights(judged: &[Judged], ms: u64, ch: i32) -> Vec<f64> {
    let end = judged.last().map_or(0, |j| j.ms);
    judged
        .iter()
        .filter(|j| j.ms + ms >= end)
        .filter_map(|j| j.coat.pet)
        .map(|(_, _, y0, y1)| f64::from(y1 - y0) / f64::from(ch))
        .collect()
}

/// THE COMPANION IS DRAWN WHOLE PAST THE ROW ABOVE'S LAST LETTER (composed r0's judges, R3a):
/// in the gallery's `context` the prompt's row lies under a powerline segment whose letters
/// end at its column 21 (its tail a background's fill to the row's end, blank cells) and over
/// a selected line lettered to its column 50. The cat comes to the prompt beside the caret
/// under the segment's letters and is fitted to its row there, and the line's typing carries
/// it on past them. Held fitted across "a gap" that was the line's end, it rested a 40x24
/// token by the caret in all 64 `context` frames, where looks9's and looks10's finals seat it
/// whole (73x44) on no letter. A gap that holds a body has ink on both sides of it
/// ([`OldPath::LineEndIsNoGap`]). Every frame the companion is on the prompt's row
/// ([`on_row`]) from its landing there keeps its box off every glyph's cell, its body off ink
/// and the caret's cell, and over the scene's last 300 ms (its rest by the caret) its box is
/// drawn whole (taller than 1.4 rows): every companion style on dark, Solarized Dark and both
/// light themes. Since a seat clears the stretch the head law reads
/// ([`OldPath::SeatClearsTheStretch`]) the cat comes down to the prompt whole and is never
/// fitted there, so the law is read at a line's end where the body would be held fitted: the
/// typing scene's line on a prompt between a sentence whose letters end at its column 43 and
/// three lettered rows under (no seat under the box), where the cat comes to rest past the
/// sentence's end whole (taller than 1.4 rows over the last 300 ms), its box on no glyph's cell
/// and its body on no ink from 600 ms (a seat reads its own row,
/// [`OldPath::SeatReadsItsOwnRow`]: reverted, the cat came there fitted, its box on the letters
/// in 8 frames). Negative control (the law reverted, the cat on dark, that prompt): held across
/// the line's end, it rests fitted (no taller than 1.05 rows). The cat there keeps its seat
/// past the sentence's end through the typing since r24 (THE WRITING COMES TO A SEAT AHEAD), so
/// the control reverts that law as well ([`OldPath::SeatAheadPulledBack`]), which pulls it in to
/// the caret's stand and back out to rest where the line's end is read.
#[test]
fn the_companion_is_drawn_whole_past_the_row_above_s_last_letter() {
    for look in [DARK, SOLARIZED_DARK, DEFAULT_LIGHT, GITHUB_LIGHT] {
        for style in COMPANIONS {
            let (judged, ch) = context_coats(style, look, &[]);
            // From its landing on the prompt's row: the replay's cat starts in the window's
            // corner and flies down across the page to the prompt, a flight between rows.
            let from = on_row(&judged, 600, 4, ch)
                .iter()
                .find(|j| j.coat.pet.is_some_and(|(_, _, y0, _)| y0 >= 3 * ch))
                .map_or(u64::MAX, |j| j.ms);
            let on = on_row(&judged, from, 4, ch);
            let s = sum(&on, from);
            let boxed = boxes_on_glyphs(&on, from, 0..=23);
            let rest = last_heights(&judged, 300, ch);
            assert!(
                s.frames >= 40 && rest.len() >= 10,
                "fixture: {style} on {} is on the prompt's row ({} frames) and at rest ({})",
                look.name,
                s.frames,
                rest.len()
            );
            assert_eq!(
                (boxed, s.on_ink, s.on_caret),
                (0, 0, 0),
                "{style} on {}: frames on the prompt's row whose box meets a glyph's cell, \
                 whose body lies on ink, on the caret: {s:?}",
                look.name
            );
            assert!(
                rest.iter().all(|&h| h > 1.4),
                "{style} on {}: at rest past the segment's letters its box is fitted, heights \
                 {rest:.2?} rows",
                look.name
            );
        }
    }
    let held_first = |old: &[OldPath]| -> (Vec<f64>, usize) {
        let (judged, ch) = fitted_then_past_the_line(old);
        let off = boxes_on_glyphs(&judged, 600, 0..=23) + sum(&judged, 600).on_ink;
        (last_heights(&judged, 300, ch), off)
    };
    let (rest, off) = held_first(&[]);
    assert!(
        off == 0 && !rest.is_empty() && rest.iter().all(|&h| h > 1.4),
        "past the sentence's end it rests whole ({rest:.2?} rows), off the letters ({off} \
         frames on a glyph's cell or on ink)"
    );
    let (rest, _) = held_first(&[OldPath::LineEndIsNoGap, OldPath::SeatAheadPulledBack]);
    assert!(
        !rest.is_empty() && rest.iter().all(|&h| h <= 1.05),
        "negative control: held across the line's end, it rests whole: {rest:.2?} rows"
    );
}

/// The gallery's `context` in the replay with the word pass off (the page's ink the companion
/// reads then the console's observation alone, no ledger span for a row the program wrote),
/// the caret a steady bar where `bar` says, the companion's laws in `old` reverted, every
/// frame's coat read.
fn context_unworded(style: &str, look: Look, bar: bool, old: &[OldPath]) -> Vec<Judged> {
    let mut r = Replay::new(style, look, TrailColor::Automatic);
    r.coat = true;
    for &law in old {
        r.fx.set_pet_old_path(law, true);
    }
    let (mut ev, tail) = Scene::Context.events_on(look);
    if bar && let Some((_, Ev::Out(first))) = ev.first_mut() {
        let mut shaped = b"\x1b[6 q".to_vec();
        shaped.extend_from_slice(first);
        *first = shaped;
    }
    r.run(&ev, tail)
}

/// A SEAT IS READ ON ITS OWN ROW ([`OldPath::SeatReadsItsOwnRow`]): a seat under the caret's
/// line is no seat where the page the console observes has ink on the seat's own row under the
/// body, whatever the ink ledger holds there. With the word pass off the ledger has no span for
/// a row a program wrote, and the whole seat's search ([`OldPath::WholeSeatOverFit`]) took
/// `context`'s selected line under the prompt for blank: the cat came down onto it beside the
/// caret, fitted there over its letters for a quarter second, and rose again across them. In
/// `context` with the word pass off, its caret a block and a bar, from 600 ms to the scene's
/// end no frame's box meets a glyph's cell on a row under the prompt's: every companion style
/// on dark, Solarized Dark and both light themes. Negative control (the law reverted alone, the
/// cat on dark, the bar): its box meets the selected line's letters.
#[test]
fn a_seat_under_the_line_reads_its_own_row() {
    let under = 5..=usize::from(crate::trail_letters_read::ROWS);
    for look in [DARK, SOLARIZED_DARK, DEFAULT_LIGHT, GITHUB_LIGHT] {
        for style in COMPANIONS {
            for bar in [false, true] {
                let judged = context_unworded(style, look, bar, &[]);
                assert!(
                    judged.iter().filter(|j| j.coat.pet.is_some()).count() >= 60,
                    "fixture: {style} on {} (bar {bar}) is drawn",
                    look.name
                );
                assert_eq!(
                    boxes_on_glyphs(&judged, 600, under.clone()),
                    0,
                    "{style} on {} (bar {bar}): frames whose box meets a glyph's cell under the \
                     prompt's row",
                    look.name
                );
            }
        }
    }
    let old = context_unworded(
        "rainbow kitty pet",
        DARK,
        true,
        &[OldPath::SeatReadsItsOwnRow],
    );
    let boxed = boxes_on_glyphs(&old, 600, under);
    assert!(
        boxed >= 10,
        "negative control: on the selected line's letters in {boxed} frames"
    );
}

/// The typing scene's line on a prompt (row 4) between a sentence whose letters end at its
/// column 43 and three lettered rows under it (no seat under the box), to 2 s after the last
/// key, the companion's laws in `old` reverted, every frame's coat read; and the replay's cell
/// height. Under the sentence no seat by the caret keeps the body whole: the cat takes the seat
/// past the sentence's end, walks on with the writing, and comes back a few cells to the seat
/// once the caret rests.
fn fitted_then_past_the_line(old: &[OldPath]) -> (Vec<Judged>, i32) {
    let below = "Below: every glyph here must stay legible! pqgyj {braces} [ok] and more";
    let mut r = Replay::new("rainbow kitty pet", DARK, TrailColor::Automatic);
    r.coat = true;
    r.fx.set_sparkle_enabled(true);
    for &law in old {
        r.fx.set_pet_old_path(law, true);
    }
    let ch = r.cell.1 as i32;
    let page = format!(
        "\x1b[2J\x1b[4Hthe quick brown fox jumps over the lazy dog\x1b[6H{below}\r\n{below}\
         \r\n{below}\x1b[5H$ "
    );
    let mut ev = vec![(0, Ev::Out(page.into_bytes()))];
    ev.extend(typed(TYPING_TEXT, 1_000, 60));
    (r.run(&ev, 2_000), ch)
}

/// A WALK'S LOOK AHEAD ENDS AT ITS STAND: the head law reads the row above ahead of a walking
/// body as far as its speed carries it in the law's ease ([`OldPath::HeadLooksAhead`]), so the
/// body is down or fitted by the time it reaches a word. A body walking a few cells back to a
/// whole seat beside a line's last letter read the letters past the seat, where it never goes:
/// it was fitted on the way and grew again at the seat, a dip and return (the companion that
/// blinks). The look ahead now stops at the stand the walk goes to
/// ([`OldPath::LookAheadStopsAtTheStand`]). In [`fitted_then_past_the_line`], once the cat has
/// first stood whole on the prompt's row, no frame on that row draws it fitted (no taller than
/// 1.05 rows). Negative control (the law reverted): frames on the way back to the seat are
/// fitted. The cat there keeps its seat past the sentence's end through the typing since r24
/// (THE WRITING COMES TO A SEAT AHEAD) and walks nowhere, so the control reverts that law as
/// well ([`OldPath::SeatAheadPulledBack`]), whose cat walks out from the caret and back.
#[test]
fn a_walk_s_look_ahead_ends_at_its_stand() {
    let fitted_after_whole = |old: &[OldPath]| -> (usize, usize) {
        let (judged, ch) = fitted_then_past_the_line(old);
        let hs: Vec<f64> = on_row(&judged, 600, 4, ch)
            .iter()
            .filter_map(|j| j.coat.pet)
            .map(|(_, _, y0, y1)| f64::from(y1 - y0) / f64::from(ch))
            .collect();
        let first = hs.iter().position(|&h| h > 1.4).unwrap_or(hs.len());
        (
            hs.len() - first,
            hs[first..].iter().filter(|&&h| h <= 1.05).count(),
        )
    };
    let (whole, fitted) = fitted_after_whole(&[]);
    assert!(
        whole >= 60,
        "fixture: the cat stands whole on the prompt's row ({whole} frames)"
    );
    assert_eq!(
        fitted, 0,
        "frames fitted after the cat first stood whole past the line"
    );
    let (_, fitted) = fitted_after_whole(&[
        OldPath::LookAheadStopsAtTheStand,
        OldPath::SeatAheadPulledBack,
    ]);
    assert!(
        fitted > 0,
        "negative control: the walk back to the seat was never fitted"
    );
}

/// The gallery's `enter-bottom` in the replay from its Return to 1.5 s after it, the companion's
/// laws in `old` reverted, every frame's coat read; and the replay's cell height.
fn enter_bottom_coats(
    style: &str,
    look: Look,
    old: &[OldPath],
    output: &[u8],
) -> (Vec<Judged>, i32, u64) {
    let enter = 1_000 + 60 * TYPING_TEXT.len() as u64 + 200;
    let mut r = Replay::new(style, look, TrailColor::Automatic);
    r.coat = true;
    r.fx.set_sparkle_enabled(true);
    for &law in old {
        r.fx.set_pet_old_path(law, true);
    }
    let ch = r.cell.1 as i32;
    let (mut ev, _) = Scene::EnterBottom.events_on(look);
    let last = ev.last_mut().expect("fixture: the scene's events");
    assert!(
        matches!(&last.1, Ev::Out(b) if b.as_slice() == ENTER_OUTPUT),
        "fixture: the scene ends on its output and the new prompt"
    );
    last.1 = Ev::Out(output.to_vec());
    (r.run(&ev, 1_500), ch, enter)
}

/// The gallery's `enter-bottom` output after its Return: the line's output and the new prompt.
const ENTER_OUTPUT: &[u8] = b"the quick brown fox\r\n$ ";

/// A SEAT PAST A LINE'S END KEEPS THE BODY WHOLE (composed r1's judges, R3a's `e470` half): the
/// line typed on the grid's last row and entered scrolls up with its output under it, and the
/// cat bounds back along the last row to a seat just past the output line's last letter. The
/// seat read the row above for the head a twentieth of a cell before the body, the head law as
/// far as a stretch drawn from its centre reaches (`HEAD_STRETCH_PAD`), so the law fitted
/// the body it seated there and, judging the bound's whole way, from its take-off: a 42x23
/// token mid-leap over blank cells (composed r1's `enter-bottom.e470`, 6 of 24 frames; looks9's
/// and looks10's finals, captured a phase earlier, 0 of 16), at rest beside the caret's line
/// for good. A seat now clears the letters before it by the stretch's reach
/// ([`OldPath::SeatClearsTheStretch`]). From the Return to 1.5 s after it, every frame the
/// companion is on the last row ([`on_row`]) draws its box whole (taller than 1.1 rows; a
/// fitted body is 0.88-0.92) and on no glyph's cell: every companion style on dark, Solarized
/// Dark and both light themes, with the scene's output and with that output ending on a full
/// stop (ink the head law reads that is no letter, which the seat read as blank: the seat past
/// the `x` lay under the `.` and the body was drawn down or fitted there). Negative control
/// (the law reverted alone, the flat rainbow's cat on dark): frames on the last row are fitted.
#[test]
fn a_seat_past_a_line_s_end_keeps_the_body_whole() {
    let last = i32::from(crate::trail_letters_read::ROWS) - 1;
    let heights = |judged: &[Judged], from: u64, ch: i32| -> (Vec<f64>, usize) {
        let on = on_row(judged, from, last, ch);
        let h = on
            .iter()
            .filter_map(|j| j.coat.pet)
            .map(|(_, _, y0, y1)| f64::from(y1 - y0) / f64::from(ch))
            .collect();
        (h, boxes_on_glyphs(&on, from, 0..=usize::MAX))
    };
    for look in [DARK, SOLARIZED_DARK, DEFAULT_LIGHT, GITHUB_LIGHT] {
        for style in COMPANIONS {
            for output in [ENTER_OUTPUT, b"the quick brown fox.\r\n$ "] {
                let (judged, ch, enter) = enter_bottom_coats(style, look, &[], output);
                let (h, boxed) = heights(&judged, enter, ch);
                assert!(
                    h.len() >= 40,
                    "fixture: {style} on {} stays on the last row ({} frames)",
                    look.name,
                    h.len()
                );
                let fitted = h.iter().filter(|&&h| h <= 1.1).count();
                assert_eq!(
                    (fitted, boxed),
                    (0, 0),
                    "{style} on {} (output {:?}): frames on the last row fitted, and on a \
                     glyph's cell; heights {:.2?}",
                    look.name,
                    String::from_utf8_lossy(output),
                    h
                );
            }
        }
    }
    let (old, ch, enter) = enter_bottom_coats(
        "flat rainbow",
        DARK,
        &[OldPath::SeatClearsTheStretch],
        ENTER_OUTPUT,
    );
    let (h, _) = heights(&old, enter, ch);
    assert!(
        h.iter().filter(|&&h| h <= 1.1).count() >= 10,
        "negative control: seated a twentieth of a cell past the line, the body is never \
         fitted: {h:.2?}"
    );
}

/// What [`Scene::Wrapped`] shows of the companion on `look`, its laws in `old` reverted, keys
/// through the console: the highest the wrap's crossing carries its box's foot (frame px, the
/// least `y1` from its take-off off the line's first row to the frame it is down on the second),
/// the cell height, and the frames from the one it is down on to the scene's end whose drawn
/// box meets a glyph's cell on the first row or whose body lies on that row's ink.
fn wrapped_coat(style: &str, look: Look, old: &[OldPath]) -> (i32, i32, usize) {
    let judged = coats_driven(style, look, Scene::Wrapped, old, Drive::Console);
    let ch = Replay::new(style, look, TrailColor::Automatic).cell.1 as i32;
    let wrap = TYPING_FROM + 60 * (usize::from(crate::trail_letters_read::COLS) - 2) as u64;
    let feet: Vec<(u64, i32)> = judged
        .iter()
        .filter_map(|j| j.coat.pet.map(|(.., y1)| (j.ms, y1)))
        .collect();
    // Take-off: the first frame after the wrap whose box's foot rose; down: the first after it
    // whose foot stands where the frame before's did, on the second row or drawn down under it.
    let off = feet
        .windows(2)
        .find(|w| w[1].0 > wrap && w[1].1 < w[0].1)
        .map_or(u64::MAX, |w| w[1].0);
    let down = feet
        .windows(2)
        .find(|w| w[1].0 > off && w[1].1 == w[0].1 && w[1].1 > 5 * ch + ch / 2)
        .map_or(u64::MAX, |w| w[1].0);
    let apex = feet
        .iter()
        .filter(|(ms, _)| (off..down).contains(ms))
        .map(|&(_, y1)| y1)
        .min()
        .unwrap_or(i32::MAX);
    let on_the_line = judged
        .iter()
        .filter(|j| {
            j.ms >= down
                && (j.coat.box_cells.iter().any(|c| c.0 == 4)
                    || j.coat.cells.iter().any(|c| c.0 == 4))
        })
        .count();
    (apex, ch, on_the_line)
}

/// A COMPANION CROSSING A WRAP AND RESTING BESIDE THE CARET ON THE LINE'S SECOND ROW KEEPS OFF
/// THE LINE (r24 K7-4, K7-6): a line typed past the margin onto a second row and stopped there
/// ([`Scene::Wrapped`]). The wrap's crossing carries the body high over the line's first row,
/// its box's foot at the bound's apex no more than a quarter row into that row: the landing's
/// head law is judged and eased in for the crossing's last `HEAD_EASE_S` only
/// ([`OldPath::SinkAtTheLanding`]). Once down on the second row, through its run with the writing
/// and its rest beside the caret under the line's full first row, no frame's drawn box meets a
/// glyph's cell on the first row or lays the body on its ink: its head is drawn down its row
/// ([`aterm_effects::kitty_pet::head_law`]). Every companion style, on dark, Solarized Dark,
/// GitHub Light and Default Light. (Coming down through the first row in the crossing's last
/// frames, its body passes behind letters knocked out of it, a body in motion with the writing,
/// r24 K7-1.) Negative controls, each alone: with the landing's sink from the take-off
/// ([`OldPath::SinkAtTheLanding`]) the arc flies 0.65 of a row over its path instead of 1.43 and
/// the box's foot stays three quarters of a row into the first row; without the head law
/// ([`OldPath::HeadUnderLetters`]) its head lies on the first row's letters beside the caret.
#[test]
fn a_companion_crossing_a_wrap_keeps_off_the_line() {
    for (style, look) in
        COMPANIONS
            .into_iter()
            .zip([DARK, SOLARIZED_DARK, GITHUB_LIGHT, DEFAULT_LIGHT])
    {
        let (apex, ch, on_the_line) = wrapped_coat(style, look, &[]);
        assert!(
            apex <= 4 * ch + ch / 4,
            "{style} on {}: the crossing's apex left its box's foot {:.2} rows into the line's \
             first row",
            look.name,
            f64::from(apex - 4 * ch) / f64::from(ch)
        );
        assert_eq!(
            on_the_line, 0,
            "{style} on {}: frames down on the second row whose box meets the first row's \
             letters or whose body lies on their ink",
            look.name
        );
    }
    let (low, ch, _) = wrapped_coat("rainbow kitty pet", DARK, &[OldPath::SinkAtTheLanding]);
    assert!(
        low > 4 * ch + ch / 4,
        "negative control: with the landing's sink from the take-off the apex still cleared \
         the line ({:.2} rows into it)",
        f64::from(low - 4 * ch) / f64::from(ch)
    );
    let (_, _, headed) = wrapped_coat("rainbow kitty pet", DARK, &[OldPath::HeadUnderLetters]);
    assert!(
        headed > 0,
        "negative control: without the head law the companion kept its head off the first row"
    );
}
