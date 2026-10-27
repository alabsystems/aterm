// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The companion keeps off the letters: the trail gallery's scenes replayed through the shipping
// effects pipeline and the real renderer (`trail_letters_read`'s replay), every 16 ms frame, the
// pet's drawn body (the knock layer's rects that meet its box) read against every lettered
// cell's ink, the caret's cell and the word icons on glass.

use crate::trail_letters_read::{
    Coat, DARK, DEFAULT_LIGHT, GITHUB_LIGHT, Judged, Look, Replay, SOLARIZED_DARK, Scene,
};
use aterm_effects::cursor_fx::TrailColor;
use aterm_effects::kitty_pet::OldPath;

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
/// cat as it comes down at the prompt. (The first keys' spring,
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
        let start = on_letters(&old(&[OldPath::FirstKeysAhead]), first_key, first_key + 700);
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
