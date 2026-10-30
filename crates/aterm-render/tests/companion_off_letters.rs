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
/// off the keys too, so alone the old landing lies on them 7 frames, not 10. (The first keys' spring,
/// [`OldPath::FirstKeysSpring`], is not what keeps this replay's start clear: the landing's
/// re-aim is; `kitty_pet`'s `wall_burst` guards it.)
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
    typed_under_a_lettered_row(style, look, 4, (above, below), old).0
}

/// The prompt `$ ` on row `prompt` (0-based) under `above`, with `below` on the row under it
/// where there is one, the companion coming up beside it, and the typing scene's line typed at
/// 60 ms a key from 1 s, keys through the console, every frame's coat read, each letter's own
/// reading too ([`Replay::own`]); and the replay's cell height.
fn typed_under_a_lettered_row(
    style: &str,
    look: Look,
    prompt: u16,
    (above, below): (&str, &str),
    old: &[OldPath],
) -> (Vec<Judged>, i32) {
    let mut r = Replay::new(style, look, TrailColor::Automatic);
    r.coat = true;
    r.own = true;
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
    ev.extend(typed(TYPING_TEXT, 1_000, 60));
    (r.run(&ev, 1_600), ch)
}

/// The letters of row `row` judged from `from` ms that read under their bar, the lower of
/// their own reading and [`crate::trail_letters_read::BAR`], and the worst of them with its own
/// reading.
fn letters_under_their_bar(judged: &[Judged], from: u64, row: usize) -> (usize, f64, f64) {
    let mut under = 0;
    let (mut worst, mut own) = (f64::INFINITY, f64::INFINITY);
    for l in judged
        .iter()
        .filter(|j| j.ms >= from)
        .flat_map(|j| j.letters.iter().filter(|l| l.row == row))
    {
        if l.judged() < l.own.min(crate::trail_letters_read::BAR) - 1e-6 {
            under += 1;
        }
        if l.judged() < worst {
            (worst, own) = (l.judged(), l.own);
        }
    }
    (under, worst, own)
}

/// The frames from `from` ms, once the head law first holds the body in row `row` (cell height
/// `ch`: its box's top at or under the row's top edge while it stands on the row, its feet in
/// the row's lower half or drawn down into the row under), in which the box rises past that
/// edge again while it stands there: a held body let go and drawn whole, to be drawn down
/// again at the next word.
fn rises_past_the_row(judged: &[Judged], from: u64, row: i32, ch: i32) -> usize {
    let (top, feet) = (row * ch, (row * ch + ch / 2)..((row + 2) * ch));
    let tops: Vec<i32> = judged
        .iter()
        .filter(|j| j.ms >= from)
        .filter_map(|j| j.coat.pet)
        .filter(|&(_, _, _, y1)| feet.contains(&y1))
        .map(|(_, _, y0, _)| y0)
        .collect();
    tops.iter().position(|&y0| y0 >= top).map_or(0, |held| {
        tops[held..].iter().filter(|&&y0| y0 < top - ch / 4).count()
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

/// THE COMPANION'S WHOLE BOX KEEPS OFF THE LETTERS, THE ROW ABOVE ITS FEET AS WELL AS ITS
/// OWN (looks9's review): its drawn box stands 1.7 rows tall, so a cat on the prompt's row
/// reaches 0.7 of a row into the row above; with a command's output on that row (the usual
/// case: any output, then a prompt) its ears and back lay on the letters' descenders for
/// most of the typing (`gjpqy` read 1.6:1 under the dog). It is drawn down its own row
/// instead, its head in the prompt's row past the caret and its feet in the blank row under
/// it ([`aterm_effects::kitty_pet::head_law`]). From 600 ms (come to the prompt) to the
/// scene's end, no frame's drawn box meets a cell of the row above holding a glyph, by a pixel
/// or more (the gallery's `trail-companion` read), or lays the body on a letter's ink or the
/// caret's cell:
/// with a row of descenders and with a sentence above the prompt, the cat on dark and the dog
/// on Solarized Dark. Negative control: without the head law ([`OldPath::HeadUnderLetters`]
/// reverted alone) the box lies on the row above's letters (186 frames under `gjpqy`, 73
/// under the sentence, measured by ink).
///
/// Under ragged output above (words six cells apart, a long listing's columns, words 24 cells
/// apart), the gaps wider than the body: the law reads the row above as far past the body,
/// either side, as its ease can take it ([`OldPath::HeadLooksAhead`]; reverted alone, the first
/// keys' hop out of the gap the cat rests in is drawn down late and its box meets the next
/// word's first letters), and once drawn down the body stays down across a gap wider than that
/// ([`OldPath::HeadHeldAcrossGaps`]; reverted alone, under words 24 cells apart it rises whole
/// in the gap, its box past the prompt row's top, and is drawn down again at the next word).
/// Review9fix round 3's review: 13 of 211 frames on the row above's letters, at its columns
/// 10-12, 20 and 30-32.
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
            let (judged, ch) = typed_under_a_lettered_row(style, look, 4, rows, &[]);
            let s = sum(&judged, from);
            let boxed = boxes_on_glyphs(&judged, from, 3..=3);
            let rises = rises_past_the_row(&judged, from, 4, ch);
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
    );
    assert!(
        rises_past_the_row(&held, from, 4, ch) > 0,
        "negative control (the hold): the box never rose past the row once held"
    );
    let (late, _) = typed_under_a_lettered_row(
        "rainbow kitty pet",
        DARK,
        4,
        (ragged, ""),
        &[OldPath::HeadLooksAhead],
    );
    assert!(
        boxes_on_glyphs(&late, from, 3..=3) > 0,
        "negative control (the look ahead): the box never met the next word's letters: {:?}",
        sum(&late, from)
    );
}

/// THE COMPANION'S BOX KEEPS OFF A LETTERED ROW ABOVE A PROMPT ON THE GRID'S LAST ROW (review9fix
/// round 3's review): once a screen fills every prompt sits on the last row, with no row under
/// it to draw the body down into, and the head law left that row out: the box lay on the
/// output's letters directly above in every frame (211 of 211), and on Solarized Dark the
/// dog's body took them to 2.00:1 against their own 4.51. The body is fitted to its own row
/// there ([`aterm_effects::kitty_pet::head_law`], the row under read as inked). From 600 ms to
/// the scene's end, no frame's box meets a glyph's cell in the row above, lays the body on ink
/// or the caret's cell, and no letter of that row reads under the lower of its own reading and
/// the bar: under a row of descenders and under a sentence, every companion style on dark and
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
            let (judged, _) = typed_under_a_lettered_row(style, look, last, rows, &[]);
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
/// command's output directly above it, and there the body is fitted to its own row (the head
/// law on the grid's last row: the row under read as inked). From the Return to 2.5 s after it,
/// no frame's drawn box meets a cell holding a glyph on any row above the last (the echo's, the
/// numbers' and the output line's directly above), by a pixel or more, for every companion
/// style on dark, Solarized Dark and both light themes, and as much with a blank line printed
/// before the new prompt, where the row above the bound is blank and the output's line two
/// above it. Negative controls, each reverted alone: [`OldPath::RowFit`], its box meets the
/// output line above the last row (7 frames for each style and theme, review9fix round 3's
/// review); [`OldPath::HeadClearInFlight`], with the blank line, its box meets the output's
/// letters two rows above mid-bound (in the scene as the gallery has it, the fit now keeps
/// the bound in its row too); [`OldPath::ArcJudgedInTheAir`], with the blank line, the cat's
/// box meets them as the output lands after the bound's take-off (18 frames).
#[test]
fn the_companion_keeps_off_the_echo_over_a_return_at_the_last_row() {
    let enter = 1_000 + 60 * TYPING_TEXT.len() as u64 + 200;
    let run = |style: &str, look: Look, old: &[OldPath], blank_line: bool| {
        let mut r = Replay::new(style, look, TrailColor::Automatic);
        r.coat = true;
        r.fx.set_sparkle_enabled(true);
        for &law in old {
            r.fx.set_pet_old_path(law, true);
        }
        let (mut ev, _) = Scene::EnterBottom.events();
        if blank_line {
            let last = ev.last_mut().expect("fixture: the scene's events");
            assert!(
                matches!(&last.1, Ev::Out(b) if b.as_slice() == b"the quick brown fox\r\n$ "),
                "fixture: the scene ends on its output and the new prompt"
            );
            last.1 = Ev::Out(b"the quick brown fox\r\n\r\n$ ".to_vec());
        }
        r.run(&ev, 2_500)
    };
    for look in [DARK, SOLARIZED_DARK, DEFAULT_LIGHT, GITHUB_LIGHT] {
        for style in COMPANIONS {
            for blank_line in [false, true] {
                let boxed = boxes_on_glyphs(&run(style, look, &[], blank_line), enter, 0..=12);
                assert_eq!(
                    boxed, 0,
                    "{style} on {} (a blank line before the prompt: {blank_line}): frames whose \
                     box meets a glyph's cell above its feet",
                    look.name
                );
            }
        }
    }
    let old = run("flat rainbow", DARK, &[OldPath::HeadClearInFlight], true);
    assert!(
        boxes_on_glyphs(&old, enter, 0..=11) > 0,
        "negative control: the bound's box never met the output's letters two rows above"
    );
    let old = run(
        "rainbow kitty pet",
        DARK,
        &[OldPath::ArcJudgedInTheAir],
        true,
    );
    assert!(
        boxes_on_glyphs(&old, enter, 0..=11) > 0,
        "negative control: judged at take-off alone, the bound's box never met the output \
         landed two rows above"
    );
    let old = run("flat rainbow", DARK, &[OldPath::RowFit], false);
    assert!(
        boxes_on_glyphs(&old, enter, 12..=12) > 0,
        "negative control: the box never met the output line above the last row"
    );
}

/// The gallery's `steam` scene in the replay: `above` on row 3, the prompt `$ ` on row 4 and
/// `below` on row 5 from the start, the typing scene's line typed at 60 ms a key from 1 s, then
/// 300 ms on, twelve Backspaces 50 ms apart, keys driven as `drive` says, every frame's coat
/// read to 1.6 s after the last; and the replay's cell height.
fn steamed_between_lettered_rows(
    style: &str,
    look: Look,
    (above, below): (&str, &str),
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
    let page = format!("\x1b[2J\x1b[4H{above}\x1b[6H{below}\x1b[5H$ ");
    let mut ev = vec![(0, Ev::Out(page.into_bytes()))];
    ev.extend(typed(TYPING_TEXT, 1_000, 60));
    let erase_from = 1_000 + 60 * TYPING_TEXT.len() as u64 + 300;
    ev.extend((0..12u64).map(|k| {
        (
            erase_from + 50 * k,
            Ev::Key('\u{7f}', b"\x08 \x08".to_vec()),
        )
    }));
    (r.run(&ev, 1_600), ch)
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
/// round 1's `steam`): with a lettered row under the prompt too, no blank row takes the body
/// down its row, and on its row its head lay on the row above's letters (107 of 112 of the
/// gallery's `steam` frames). It is fitted to its own row instead, whole and smaller, its feet
/// where they stand ([`aterm_effects::kitty_pet::head_law`]). From 600 ms (come to the prompt)
/// to the scene's end, through the line's typing and twelve Backspaces, no frame's drawn box
/// meets a cell holding a glyph on any row, by a pixel or more (the gallery's
/// `trail-companion` read), or lays the body on a letter's ink or the caret's cell: every
/// companion style on dark, Solarized Dark and both light themes, between the gallery's own
/// rows and between descenders and a sentence, its keys typed at the window (the console's)
/// and as the gallery types them (`aterm ctl key`). Every frame the body is on the prompt's row
/// is judged ([`on_row`]): the seat law takes the cat to a seat under the box and back up to
/// the prompt's row across the row under, flights the head law does not govern; every frame,
/// those flights' included, keeps its box off the row above's letters. Negative control
/// ([`OldPath::RowFit`] reverted alone, the cat on dark, `ctl` keys): its box meets the row
/// above's letters.
#[test]
fn the_companion_keeps_off_lettered_rows_above_and_under_the_prompt() {
    let from = 600;
    for rows in STEAM_ROWS {
        for look in [DARK, SOLARIZED_DARK, DEFAULT_LIGHT, GITHUB_LIGHT] {
            for style in COMPANIONS {
                for drive in [Drive::Ctl, Drive::Console] {
                    let (judged, ch) = steamed_between_lettered_rows(style, look, rows, &[], drive);
                    let above = boxes_on_glyphs(&judged, from, 3..=3);
                    let judged = on_row(&judged, from, 4, ch);
                    let s = sum(&judged, from);
                    let boxed = boxes_on_glyphs(&judged, from, 0..=23);
                    assert!(
                        s.frames >= 40,
                        "fixture: {style} on {} between {rows:?} ({drive:?}) is on the prompt's \
                         row ({} frames)",
                        look.name,
                        s.frames
                    );
                    assert_eq!(
                        (boxed, s.on_ink, s.on_caret, above),
                        (0, 0, 0, 0),
                        "{style} on {} between {rows:?} ({drive:?}): frames on the prompt's row \
                         whose box meets a glyph's cell, whose body lies on ink, on the caret; \
                         frames anywhere whose box meets the row above's letters: {s:?}",
                        look.name
                    );
                }
            }
        }
        let (old, ch) = steamed_between_lettered_rows(
            "rainbow kitty pet",
            DARK,
            rows,
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
    }
}

/// A BODY FITTED TO ITS ROW STAYS FITTED ALONG IT ([`OldPath::FitHeldAlongTheRow`]): between a
/// long lettered row above and a row under whose letters end a dozen cells in, the cat is
/// fitted to its row where the row under holds ink, and the line's typing runs it on past
/// those letters, where a blank stretch of the row under could take it down. Drawn down into it
/// whole and fitted again at the next letters, it grew and shrank as it went (the gallery's
/// `steam` capture: the box drawn down at `bs03` and `bs06` and back up at `bs09`, review9fix
/// round 1's judge). From its first fitted frame on, no frame of the cat on dark or the dog on
/// Solarized Dark draws the box whole (taller than 1.4 rows) while the caret keeps its row, and
/// none meets a lettered cell. Negative control (the law reverted alone, the cat on dark): its
/// box is drawn whole again past the row under's letters.
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
        let page = format!("\x1b[2J\x1b[4H{above}\x1b[6H{below}\x1b[5H$ ");
        let mut ev = vec![(0, Ev::Out(page.into_bytes()))];
        ev.extend(typed(TYPING_TEXT, 1_000, 60));
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
            boxes_on_glyphs(&judged, 600, 0..=23),
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
/// dog on Solarized Dark on the prompt's row ([`on_row`]) meets a lettered cell. Negative control
/// (the law reverted alone, the cat on dark): a frame's box meets the row above's letters.
#[test]
fn a_body_held_in_its_row_is_drawn_no_taller_than_its_fit() {
    let from = 600;
    let run = |style: &str, look: Look, old: &[OldPath]| {
        let (judged, ch) =
            steamed_between_lettered_rows(style, look, STEAM_ROWS[0], old, Drive::Console);
        let on = on_row(&judged, from, 4, ch);
        assert!(
            on.len() >= 40,
            "fixture: {style} on {} rests on the prompt's row",
            look.name
        );
        boxes_on_glyphs(&on, from, 0..=23)
    };
    for (style, look) in [("rainbow kitty pet", DARK), ("rainbow dog", SOLARIZED_DARK)] {
        assert_eq!(
            run(style, look, &[]),
            0,
            "{style} on {}: frames on the prompt's row whose box meets a lettered cell",
            look.name
        );
    }
    assert!(
        run("rainbow kitty pet", DARK, &[OldPath::HeldNoTaller]) > 0,
        "negative control: stretched past its fit, the box never met the row above's letters"
    );
}
