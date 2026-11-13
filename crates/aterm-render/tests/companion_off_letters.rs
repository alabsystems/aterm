// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The companion keeps off the letters: the trail gallery's scenes replayed through the shipping
// effects pipeline and the real renderer (`trail_letters_read`'s replay), every 16 ms frame, the
// pet's drawn body (the knock layer's rects that meet its box) read against every lettered
// cell's ink, the caret's cell and the word icons on glass.

use crate::trail_letters_read::{
    COLS, Coat, DARK, DEFAULT_LIGHT, Ev, GITHUB_LIGHT, Judged, LastRowThen, Look, Replay,
    SOLARIZED_DARK, Scene, TYPING_TEXT, gallery_cell, last_row_cols, last_row_line, last_row_times,
    lays_the_bundled_cell, typed,
};
use aterm_effects::cursor_fx::TrailColor;
use aterm_effects::kitty_pet::{OldPath, art_cols, head_law};
use aterm_render::{FaceRef, FaceTarget, Renderer, Theme};
use std::ops::RangeInclusive;

/// The companion styles.
pub const COMPANIONS: [&str; 4] = [
    "rainbow + cat",
    "underline rainbow + cat",
    "rainbow + dog",
    "flat rainbow + cat",
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
    coats_driven_on(None, style, look, scene, old, drive)
}

/// [`coats_driven`] drawn in `face` at its px (`None`: the window's face at the gallery's px).
pub fn coats_driven_on(
    face: Option<(&FaceRef, f32)>,
    style: &str,
    look: Look,
    scene: Scene,
    old: &[OldPath],
    drive: Drive,
) -> Vec<Judged> {
    let grid = (scene.rows(), scene.cols());
    let mut r = match face {
        None => Replay::gridded(style, look, TrailColor::Automatic, grid.0, grid.1),
        Some((face, px)) => Replay::gridded_on(
            Renderer::from_system_with_face(Some(face), px, Theme::default())
                .expect("fixture: a face this host resolved loads"),
            style,
            look,
            TrailColor::Automatic,
            grid,
        ),
    };
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
    r.old_paths = old.to_vec();
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
        Scene::FoldRest,
        Scene::FocusWrap,
        Scene::StatusWrap,
        Scene::LastRow,
        Scene::Steam,
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
                        if frames == "all"
                            || c.ink_px > 0
                            || c.icon_px > 0
                            || c.caret_px > 0
                            || !c.box_cells.is_empty()
                        {
                            println!(
                                "  {:5} ms pet {:?} ink {} caret {} icon {} cells {:?} box {:?}",
                                j.ms, c.pet, c.ink_px, c.caret_px, c.icon_px, c.cells, c.box_cells
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
/// program's letter) is left behind. Its stand there may be flush with the `k`'s cell, and the
/// coil it gathers there to leap when the caret moves up keeps its stretch off the `k`
/// (`kitty_pet`'s `keep_stretch_off_letters`; stretched about its centre, the dog's coil on the
/// gallery's frames laid its body on the `k`'s ink at 2559 ms under Linux's bundled JetBrains
/// Mono's 13 x 29 px cells, a body 6.25 cells across). Negative control: without the law that
/// keeps it watching from beside the line ([`OldPath::WatchFromTheLine`] reverted alone) the
/// cat lies on `NORMAL`, each way, until the ink sends it off.
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
        for (style, look) in [("rainbow + cat", DARK), ("rainbow + dog", SOLARIZED_DARK)] {
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
                "rainbow + cat",
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

/// The cat's sit after the hand's last key (`kitty_pet`'s `SIT_AFTER`), and, for a cat the margin
/// found no row beside the line for, the hop off the line's last letters then and its landing.
const SAT_AND_HOPPED_MS: u64 = 1_400 + 600;

/// THE BURST NEVER LAYS A RUNNING OR RESTING COMPANION ON ITS LETTERS, ITS ICONS OR THE CARET
/// (the trail gallery's `burst`, looks8 rounds 2 and 3, r23 K6): no frame lays the
/// companion's drawn body on a lettered cell, the caret's cell or a word icon on glass (where
/// it lay on an icon, the icon yielded to it and came back: a blink), and no icon blinks, from
/// the first key to the last (the run's start, where round 2 caught the body on `echo the
/// qu` as the first keys were typed under the cat landing at the prompt, and the run to the
/// wall), from the last key to its sit and from the sit on to the scene's end, 4.6 s past the
/// last key. It waits at the wall for a wrap the hand may still type, on the blank row under
/// the line, where it slid as it took the margin (`margin_seat_off_the_line`), so the line's
/// last letters are typed past it: no bound leaves for a wrap that has not come (r23 K6-1:
/// leaving early, it hung over the line and came down on the next line's start for a wrap that
/// never came). Every frame of the run is judged as it is drawn, the letters typed in at the
/// wall included. It rests there: its drawn box keeps its place, within its breath, through
/// the scene's last second. With the cat on dark and the dog on Solarized Dark, the keys typed
/// through the console and, as the gallery types them (`aterm ctl key`), past it. Negative
/// controls: without the first keys' landing ahead of them ([`OldPath::FirstKeysAhead`]) they
/// are typed under the cat as it comes down at the prompt, with the writing's own law reverted
/// as well ([`OldPath::StillUnderWriting`], looks10 round 4): it runs a landing the writing
/// reaches off the keys too, so alone the old landing lies on them 7 frames, not 10; and
/// without the margin's slide ([`OldPath::MarginKeepsTheLine`]) the cat waits at the wall on
/// the line, the caret is typed into its cell and the hop off the line's letters at its sit
/// lies on them ([`margin_lets_the_writing_run_under`]).
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
        for (style, look) in [("rainbow + cat", DARK), ("rainbow + dog", SOLARIZED_DARK)] {
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
            for (part, from, to) in [
                ("run", first_key, last_key),
                ("wait", last_key, rested),
                ("rest", rested, end),
            ] {
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
        let old = |laws: &[OldPath]| coats_driven("rainbow + cat", DARK, Scene::Burst, laws, drive);
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
        let walled = margin_lets_the_writing_run_under(Scene::Burst, drive);
        assert!(
            walled.iter().any(|(_, s)| (s.on_ink, s.on_caret) != (0, 0)),
            "negative control ({drive:?}): without the margin's slide the burst kept the cat off \
             its letters and the caret: {walled:?}"
        );
    }
}

/// [`Scene::Burst`]'s or [`Scene::SplitBurst`]'s run and wait (from the first key to the last,
/// and from the last to [`SAT_AND_HOPPED_MS`] past it) for the cat on dark with the margin's
/// slide reverted ([`OldPath::MarginKeepsTheLine`]): each part's sum. The caret's cell law
/// ([`OldPath::CaretCellIsNoSeat`]) is kept, so the slide's own loss shows.
fn margin_lets_the_writing_run_under(scene: Scene, drive: Drive) -> Vec<(&'static str, Sum)> {
    let (ev, _) = scene.events();
    let keys: Vec<u64> = ev
        .iter()
        .filter(|(_, e)| matches!(e, crate::trail_letters_read::Ev::Key(..)))
        .map(|(at, _)| *at)
        .collect();
    let (first_key, last_key) = (keys[0], keys[keys.len() - 1]);
    let judged = coats_driven(
        "rainbow + cat",
        DARK,
        scene,
        &[OldPath::MarginKeepsTheLine],
        drive,
    );
    [
        ("run", first_key, last_key),
        ("wait", last_key, last_key + SAT_AND_HOPPED_MS),
    ]
    .into_iter()
    .map(|(part, from, to)| {
        let window: Vec<Judged> = judged
            .iter()
            .filter(|j| j.ms >= from && j.ms <= to)
            .cloned()
            .collect();
        (part, sum(&window, from))
    })
    .collect()
}

/// THE SPLIT'S BURST NEVER LAYS A RUNNING OR RESTING COMPANION ON ITS LETTERS (the trail
/// gallery's `split`, looks8 round 3, r23 K6): the burst typed on the last row but one of the
/// caret's 6-row pane, so the row under the line is the pane's last, where the body's head
/// would lie in the line. From the first key to the scene's end, 4.6 s past the last key, no
/// frame lays the companion's drawn body on a
/// lettered cell, the caret's cell or a word icon, no icon blinks, and it has come to rest by
/// the scene's last second: as it takes the margin it slides onto the row over the line
/// (`margin_seat_off_the_line`; round 3's first capture caught a bound coming down onto
/// `the` 1.65 s after the last key, to rest with its head in the line). Every frame of the run
/// is judged as it is drawn. With the cat on dark and the dog on Solarized Dark, the keys typed
/// through the console and, as the gallery types them, past it. Negative control: without the
/// margin's slide ([`OldPath::MarginKeepsTheLine`]) the cat waits at the wall on the line, the
/// caret is typed into its cell and the hop off the line's letters at its sit lies on them
/// ([`margin_lets_the_writing_run_under`]).
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
        for (style, look) in [("rainbow + cat", DARK), ("rainbow + dog", SOLARIZED_DARK)] {
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
            for (part, from, to) in [
                ("run", first_key, last_key),
                ("wait", last_key, rested),
                ("rest", rested, end),
            ] {
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
        let walled = margin_lets_the_writing_run_under(scene, drive);
        assert!(
            walled.iter().any(|(_, s)| (s.on_ink, s.on_caret) != (0, 0)),
            "negative control ({drive:?}): without the margin's slide the split's burst kept the \
             cat off its letters and the caret: {walled:?}"
        );
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
/// height. Each of `above` and `below` fits the grid's row ([`words_apart`]).
fn typed_under_a_lettered_row(
    style: &str,
    look: Look,
    prompt: u16,
    (above, below): (&str, &str),
    old: &[OldPath],
    letters: bool,
) -> (Vec<Judged>, i32) {
    for line in [above, below] {
        assert!(
            line.chars().count() <= usize::from(COLS),
            "fixture: {line:?} fits the grid's {COLS} columns: a longer row wraps onto the next"
        );
    }
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

/// `words` a `gap` of blank cells apart after `lead` blank cells, as many of them as the grid's
/// row holds ([`COLS`]): a longer row wraps, and its tail lands on the row under it, the
/// prompt's under a row above it. The look ahead's control row, two gaps of seven cells in
/// (Linux's 13 x 29 px cell, a body 6.25 cells across), ran 10 cells past the grid's 72: its
/// `yz` landed on the prompt's row under the cat's stand by the caret, so the cat sat two rows
/// under the prompt, under no letter, and the control never met one (on the gallery's 14 x 26
/// px cell the two cells past the grid fell under the prompt's `$ `).
fn words_apart(lead: usize, words: &[&str], gap: usize) -> String {
    let mut row = " ".repeat(lead);
    for (i, word) in words.iter().enumerate() {
        let sep = if i == 0 { 0 } else { gap };
        if row.chars().count() + sep + word.chars().count() > usize::from(COLS) {
            break;
        }
        row.push_str(&" ".repeat(sep));
        row.push_str(word);
    }
    row
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
/// out of its body ([`crate::companion_knock_out`]). Six keys at the typing scenes' 60 ms, as the
/// effects' own `FIRST_KEYS_LAG_MS` is six at the burst's 50: read at five, the caret's cell was
/// still under the tail of a cat getting going at the sixth key's frames (37 and 3 px at 312 and
/// 328 ms after the first key, under a row of descenders).
const FIRST_KEYS_LAG_MS: u64 = 6 * 60;

/// `judged` less the frames in which the first letters of a run typed from rest on row `row`,
/// its first key at `first_key` ms, pass over the companion getting going
/// ([`FIRST_KEYS_LAG_MS`]): those within that time of the first key whose box and body meet
/// that row's letters and no other row's. The caret leads those letters onto the body at rest
/// beside it, so its cell is passed over too: a cat stood a cell past the caret at rest, and the
/// first key brought the caret onto its tail until its walk took it clear (looks8 K7-1's run
/// start; the replay read no caret past a row's written cells until 2026-11-07, and so never
/// saw it).
fn past_the_first_keys(judged: &[Judged], first_key: u64, row: usize) -> Vec<Judged> {
    judged
        .iter()
        .filter(|j| {
            let c = &j.coat;
            !((first_key..first_key + FIRST_KEYS_LAG_MS).contains(&j.ms)
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
/// Under ragged output above (words the body's width apart, rounded up to a cell, a long
/// listing's columns as far apart, words 24 cells apart, each row as many words as the grid's
/// row holds, [`words_apart`]), the gaps wider than the body: the law reads the row above as
/// far past the body, either side, as its ease can take it ([`OldPath::HeadLooksAhead`];
/// reverted alone, the box of the cat getting going out of the gap it rests in meets the next
/// word's first letters: the ragged words two gaps' width in, so the stand by the prompt's
/// caret lies in a gap; 6 frames on `abcd` under Linux's 13 x 29 px cell), at the speed the run
/// can reach in the ease and past a stand the writing carries on
/// ([`OldPath::LookAheadAtTopSpeed`]; reverted alone, under words 24 cells apart, the first 24
/// cells in, the cat speeding up off the first keys meets `efgh` before its sink is done, r24
/// K7-1), and once drawn down the body stays down across a gap wider than that
/// ([`OldPath::HeadHeldAcrossGaps`]; reverted alone, under words 24 cells apart it rises whole
/// in the gap, its box past the prompt row's top, and is drawn down again at the next word).
/// Each look ahead's scene is replayed with every law kept as well, and there no frame's box
/// meets a glyph's cell in the row above. Review9fix round 3's review: 13 of 211 frames on the
/// row above's letters, at its columns 10-12, 20 and 30-32.
#[test]
fn the_companion_keeps_off_a_lettered_row_above_the_prompt() {
    let gj = "gjpqy ".repeat(12);
    let fox = "the quick brown fox jumps over the lazy dog";
    // The ragged rows' gaps: the body's width rounded up to a cell ([`art_cols`]), six cells
    // on the gallery's 14 x 26 px cells (a body 5.2 cells across) and seven under Linux's
    // bundled JetBrains Mono's 13 x 29 (6.25 across), where six held no body.
    let cell = Replay::new("off", DARK, TrailColor::Automatic).cell;
    let gap = art_cols(cell.0 as u16, cell.1 as u16).ceil() as usize;
    let words = ["abcd", "efgh", "ijkl", "mnop", "qrst", "uvwx", "yz"];
    let ragged = words_apart(0, &words, gap);
    let listing = words_apart(
        0,
        &["ls -la", "total 48", "drwxr-xr-x", "staff", "Oct 29"],
        gap,
    );
    let wide = words_apart(0, &["abcd", "efgh", "ijkl"], 24);
    let gapped = words_apart(2 * gap, &words, gap);
    let wide_gapped = words_apart(24, &["efgh", "ijkl"], 24);
    let from = 600;
    for rows in [
        (gj.as_str(), ""),
        (fox, ""),
        (ragged.as_str(), ""),
        (listing.as_str(), ""),
        (wide.as_str(), ""),
    ] {
        for (style, look) in [
            ("rainbow + cat", DARK),
            ("underline rainbow + cat", DARK),
            ("rainbow + dog", SOLARIZED_DARK),
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
        let old =
            typed_between_lettered_rows("rainbow + cat", DARK, rows, &[OldPath::HeadUnderLetters]);
        let old_boxed = boxes_on_glyphs(&old, from, 3..=3);
        assert!(
            old_boxed >= 10,
            "negative control between {rows:?}: its head among the row above's letters in \
             {old_boxed} frames: {:?}",
            sum(&old, from)
        );
    }
    let (held, ch) = typed_under_a_lettered_row(
        "rainbow + cat",
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
    for (scene, law, what) in [
        (&gapped, OldPath::HeadLooksAhead, "the look ahead"),
        (
            &wide_gapped,
            OldPath::LookAheadAtTopSpeed,
            "the look ahead at the run's top speed",
        ),
    ] {
        let boxed = |old: &[OldPath]| {
            let (judged, _) =
                typed_under_a_lettered_row("rainbow + cat", DARK, 4, (scene, ""), old, false);
            (boxes_on_glyphs(&judged, from, 3..=3), sum(&judged, from))
        };
        let (kept, s) = boxed(&[]);
        assert_eq!(
            kept, 0,
            "{what}'s scene under {scene:?}, every law kept: frames whose box meets a glyph's \
             cell in the row above: {s:?}"
        );
        let (old, s) = boxed(&[law]);
        assert!(
            old > 0,
            "negative control ({what}): the box never met the next word's letters: {s:?}"
        );
    }
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
        ("rainbow + dog", SOLARIZED_DARK),
        ("rainbow + dog", GITHUB_LIGHT),
    ]);
    for rows in [(gj.as_str(), ""), (fox, "")] {
        for &(style, look) in &runs {
            let (judged, _) = typed_under_a_lettered_row(style, look, last, rows, &[], true);
            let s = sum(
                &past_the_first_keys(&judged, TYPING_FROM, usize::from(last)),
                from,
            );
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
        "rainbow + dog",
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
/// law judges a body as the page under it changes ([`OldPath::JudgedAsThePageChanges`]); and
/// with that line and the new prompt 24 cells in, the caret beside the resting cat.
/// Negative controls, each reverted alone: [`OldPath::RowFit`], under the long line its box
/// meets the line above the last row; [`OldPath::JudgedAsThePageChanges`], with the prompt 24
/// cells in, the long line lies under the cat's whole box until it next moves (156 frames, to
/// the scene's end, on 13 x 29, 14 x 26 and 11 x 28 px cells: the effects' replay); with the
/// prompt at the row's start the cat gathers to leave at once and reads its way as it does
/// ([`OldPath::WayBeforeSettingOff`]), judged there after 7 frames in that replay (32, half a
/// second, with that law reverted too); [`OldPath::HeadClearInFlight`], with the blank line, its
/// box meets the output's letters two rows above mid-bound (in the scene as the gallery has it,
/// the fit now keeps the bound in its row too); [`OldPath::ArcJudgedInTheAir`], with the blank
/// line, the cat's box meets them as the output lands after the bound's take-off (18 frames).
#[test]
fn the_companion_keeps_off_the_echo_over_a_return_at_the_last_row() {
    let enter = 1_000 + 60 * TYPING_TEXT.len() as u64 + 200;
    let long = format!("{:.70}\r\n$ ", "the quick brown fox ".repeat(4));
    // The long line, the new prompt 24 cells in: the caret lands beside the resting cat.
    let under = format!("{:.70}\r\n{:24}$ ", "the quick brown fox ".repeat(4), "");
    let outputs: [&[u8]; 4] = [
        b"the quick brown fox\r\n$ ",
        b"the quick brown fox\r\n\r\n$ ",
        long.as_bytes(),
        under.as_bytes(),
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
        "flat rainbow + cat",
        DARK,
        &[OldPath::HeadClearInFlight],
        outputs[1],
    );
    assert!(
        boxes_on_glyphs(&old, enter, 0..=11) > 0,
        "negative control: the bound's box never met the output's letters two rows above"
    );
    let old = run(
        "rainbow + cat",
        DARK,
        &[OldPath::ArcJudgedInTheAir],
        outputs[1],
    );
    assert!(
        boxes_on_glyphs(&old, enter, 0..=11) > 0,
        "negative control: judged at take-off alone, the bound's box never met the output \
         landed two rows above"
    );
    let old = run("flat rainbow + cat", DARK, &[OldPath::RowFit], outputs[2]);
    assert!(
        boxes_on_glyphs(&old, enter, 12..=12) > 0,
        "negative control: the box never met the long line above the last row"
    );
    let old = run(
        "rainbow + cat",
        DARK,
        &[OldPath::JudgedAsThePageChanges],
        outputs[3],
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
    steamed_then(style, look, page, old, drive, &[])
}

/// A program's bytes after the steam scene's Backspaces, each with when it lands (ms after the
/// last Backspace).
type Then<'a> = &'a [(u64, &'a [u8])];

/// [`steamed_between_lettered_rows`], and each of `then` landing as it says, the coats read to
/// 1.6 s after the last.
fn steamed_then(
    style: &str,
    look: Look,
    page: &str,
    old: &[OldPath],
    drive: Drive,
    then: Then<'_>,
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
    for &(after, bytes) in then {
        ev.push((erase_from + 50 * 11 + after, Ev::Out(bytes.to_vec())));
    }
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
    /// under its line are its own and the cat keeps its line's row
    /// ([`OldPath::FullScreenKeepsItsRow`]): at a seat along it, or fitted there by the caret.
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
            "rainbow + cat",
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
        "rainbow + cat",
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
    for (style, look) in [("rainbow + cat", DARK), ("rainbow + dog", SOLARIZED_DARK)] {
        let (fitted, whole, boxed) = whole_after_fit(style, look, &[]);
        assert_eq!(
            (fitted, whole, boxed),
            (1, 0, 0),
            "{style} on {}: fitted at all, frames drawn whole after its first fitted frame, \
             frames whose box meets a lettered cell",
            look.name
        );
    }
    let (fitted, whole, _) = whole_after_fit("rainbow + cat", DARK, &[OldPath::FitHeldAlongTheRow]);
    assert!(
        fitted == 1 && whole > 0,
        "negative control: never drawn whole after its first fitted frame ({whole} frames)"
    );
}

/// A BODY HELD IN ITS ROW IS DRAWN NO TALLER THAN ITS FIT ([`OldPath::HeldNoTaller`],
/// [`OldPath::DrawnDownNoTaller`]): in the `steam` scene typed at the window, a cat fitted to
/// the prompt's row between two lettered rows has 0.08 of a row over its ears, and a pose's
/// stretch (a leap's take-off, a purr's swell, an ear's twitch up) took them 3 px into the row
/// above's letters; a cat drawn down its row has as little, and a leap straight down from it
/// took them 4 px in. With the laws no frame of the cat on dark or the dog on Solarized Dark on
/// the prompt's row ([`on_row`]) meets a lettered cell, where no seat in reach draws the body
/// whole ([`Steam::Deep`]), on the alternate screen ([`Steam::FullScreen`]), and there under a
/// row lettered past the seats along the prompt's row, the cat held in it by the caret, when
/// the program moves the caret five rows down and back after the Backspaces, three times down
/// and twice up a second apart (vim's `5j` and `5k`), and the cat leaps straight down from the
/// prompt's row, fitted the first time and drawn down after, and back up into it; there no
/// frame at all, in the air or on the ground, meets the row above's letters. Negative controls
/// (the cat on dark, in those leaps): the stretch takes its box into the row above's letters,
/// at every take-off, touchdown and landing with [`OldPath::HeldNoTaller`] reverted (eight
/// frames or more; 11 under Linux's bundled JetBrains Mono, 13 x 29 px cells), at each
/// touchdown back into the drawn-down row with [`OldPath::DrawnDownNoTaller`] reverted (two or
/// more; 2 on macOS), and at each take-off down from that row as well with it and
/// [`OldPath::CrossingRisesIntoTheLine`] reverted: a take-off down is a crossing off a row the
/// body was held on, and the crossing law draws it no taller too (four or more; 6 on macOS, the
/// count [`OldPath::DrawnDownNoTaller`] alone took before the crossing law joined it). The
/// effects' own `a_body_held_fitted_or_drawn_down_is_drawn_no_taller_than_its_fit_on_every_face`
/// judges the leaps, and the three controls, on 13 x 29, 14 x 26 and 11 x 28 px cells.
#[test]
fn a_body_held_in_its_row_is_drawn_no_taller_than_its_fit() {
    let from = 600;
    // Lettered past the seats along the prompt's row (the body's width short of the margin),
    // over a row under whose letters end before the caret's column after the Backspaces.
    let held = (
        "the quick brown fox jumps over the lazy dog, every glyph legible here",
        "Below: pqgyj",
    );
    let down: &[u8] = b"\x1b[10;15H";
    let up: &[u8] = b"\x1b[5;15H";
    let moves: Then<'_> = &[
        (300, down),
        (1_300, up),
        (2_300, down),
        (3_300, up),
        (4_300, down),
    ];
    let scenes: [(_, _, Then<'_>); 3] = [
        (STEAM_ROWS[0], Steam::Deep, &[]),
        (STEAM_ROWS[0], Steam::FullScreen, &[]),
        (held, Steam::FullScreen, moves),
    ];
    // Frames on the prompt's row whose box meets a lettered cell; and, where the program moves
    // the caret, frames whose box meets the row above's letters.
    let run = |style: &str, look: Look, scene: usize, old: &[OldPath]| {
        let (rows, steam, then) = scenes[scene];
        let page = steam_page(rows, steam);
        let (judged, ch) = steamed_then(style, look, &page, old, Drive::Console, then);
        let on = past_the_first_keys(&on_row(&judged, from, 4, ch), TYPING_FROM, 4);
        assert!(
            on.len() >= 40,
            "fixture: {style} on {} ({steam:?}, {then:?}) rests on the prompt's row",
            look.name
        );
        let above = if then.is_empty() {
            0
        } else {
            boxes_on_glyphs(&judged, from, 3..=3)
        };
        (boxes_on_glyphs(&on, from, 0..=23), above)
    };
    for (scene, case) in scenes.iter().enumerate() {
        for (style, look) in [("rainbow + cat", DARK), ("rainbow + dog", SOLARIZED_DARK)] {
            assert_eq!(
                run(style, look, scene, &[]),
                (0, 0),
                "{style} on {} ({case:?}): frames on the prompt's row whose box meets a lettered \
                 cell, frames whose box meets the row above's letters as it leaps",
                look.name
            );
        }
    }
    let controls: [(&[OldPath], usize); 3] = [
        (&[OldPath::HeldNoTaller], 8),
        (&[OldPath::DrawnDownNoTaller], 2),
        (
            &[
                OldPath::DrawnDownNoTaller,
                OldPath::CrossingRisesIntoTheLine,
            ],
            4,
        ),
    ];
    for (old, bites) in controls {
        let (_, above) = run("rainbow + cat", DARK, 2, old);
        assert!(
            above >= bites,
            "negative control ({old:?}): stretched past its fit, the box met the row above's \
             letters in {above} frames"
        );
    }
}

/// A leap of the caret far from the body on a pane past the rows or the columns the console's
/// map reads at once ([`a_body_far_from_the_caret_reads_its_own_rows`]): what it is; the pane,
/// rows by columns; whether it comes after the `steam` scene's typing and Backspaces, or from
/// rest, typing no key; where the caret rests on the prompt's row and where it leaps to (rows
/// and columns, counted from 1); and how many frames, at the fewest, the negative control's box
/// meets the letters the head law keeps it off ([`kept_off`]) at the window and as the gallery
/// types (from rest: the first).
type FarLeap = (
    &'static str,
    (u16, u16),
    bool,
    (u16, u16),
    (u16, u16),
    [usize; 2],
);

/// The leaps far from the body, each control's floor two thirds of the fewest frames it was
/// measured at on Linux arm64, 2026-11-07, under Linux's bundled JetBrains Mono (13 x 29 px
/// cells): 38 and 43 from rest; 9 at the window and 11 as the gallery types after the
/// Backspaces on the tall pane, 37 and 39 on the wide one; 38 from rest on a 160-row pane's
/// 85th row; and 10 at a 100-row pane's foot, 88 rows up, standing on the prompt's row.
const FAR_LEAPS: [FarLeap; 6] = [
    (
        "40j from rest",
        (100, 72),
        false,
        (5, 58),
        (45, 58),
        [25, 0],
    ),
    (
        "58j from rest",
        (100, 72),
        false,
        (5, 58),
        (63, 58),
        [28, 0],
    ),
    (
        "40j after the Backspaces",
        (100, 72),
        true,
        (5, 15),
        (45, 15),
        [6, 7],
    ),
    (
        "5j to the 290th column after the Backspaces",
        (14, 300),
        true,
        (5, 15),
        (10, 290),
        [24, 26],
    ),
    (
        "40j from rest on a 160-row pane's 85th row",
        (160, 72),
        false,
        (85, 58),
        (125, 58),
        [25, 0],
    ),
    (
        "88k from rest at a 100-row pane's foot",
        (100, 72),
        false,
        (98, 58),
        (10, 58),
        [6, 0],
    ),
];

/// The frames from `from` ms in which `leap`'s box meets glyphs the head law keeps the body off
/// (cells `ch` px tall): the row's beside the prompt's that its leaps do not cross (the row
/// above's for a leap down, the row under's for one up), and for a leap up, the row above's
/// while the box's foot stands on the prompt's row, before the flight crosses it.
fn kept_off(judged: &[Judged], from: u64, leap: FarLeap, ch: i32) -> usize {
    let prompt = usize::from(leap.3.0) - 1;
    if leap.4.0 > leap.3.0 {
        return boxes_on_glyphs(judged, from, prompt - 1..=prompt - 1);
    }
    let ground = (prompt as i32 + 1) * ch;
    judged
        .iter()
        .filter(|j| {
            j.ms >= from
                && j.coat.box_cells.iter().any(|c| {
                    c.0 == prompt + 1
                        || (c.0 == prompt - 1 && j.coat.pet.is_some_and(|p| p.3 >= ground))
                })
        })
        .count()
}

/// `leap` on the alternate screen, its prompt on its seat's row and the rows over and under
/// that one lettered to the margin, or blank, keys driven as `drive` says, the laws in `old` reverted: from rest the caret goes to its seat 300
/// ms after the page and leaps there and back twice, 1.2 s apart, the coats read to 3 s after
/// the last; after the Backspaces it leaps 300 ms after the last, back 1.2 s later, and there
/// and back again 1.2 s apart, the coats read to 1.6 s after the last. And the replay's cell
/// height.
fn far_from_the_caret(
    style: &str,
    look: Look,
    (leap, lettered): (FarLeap, bool),
    old: &[OldPath],
    drive: Drive,
) -> (Vec<Judged>, i32) {
    let (_, (rows, cols), typed_first, from, to, _) = leap;
    let mut r = Replay::gridded(style, look, TrailColor::Automatic, rows, cols);
    r.coat = true;
    r.keys_unnoted = drive != Drive::Console;
    r.fx.set_sparkle_enabled(true);
    for &law in old {
        r.fx.set_pet_old_path(law, true);
    }
    let ch = r.cell.1 as i32;
    let row: String = if lettered {
        "the quick brown fox jumps over the lazy dog "
            .repeat(usize::from(cols))
            .chars()
            .take(usize::from(cols))
            .collect()
    } else {
        String::new()
    };
    let at = |(r, c): (u16, u16)| format!("\x1b[{r};{c}H").into_bytes();
    let page = format!(
        "\x1b[?1049h\x1b[2J\x1b[{}H{row}\x1b[{}H{row}\x1b[{}H$ ",
        from.0 - 1,
        from.0 + 1,
        from.0
    );
    let mut ev = vec![(0, Ev::Out(page.into_bytes()))];
    let tail = if typed_first {
        ev.extend(typed(TYPING_TEXT, TYPING_FROM, 60));
        let erase_from = TYPING_FROM + 60 * TYPING_TEXT.len() as u64 + 300;
        ev.extend((0..12u64).map(|k| {
            (
                erase_from + 50 * k,
                Ev::Key('\u{7f}', b"\x08 \x08".to_vec()),
            )
        }));
        let last = erase_from + 50 * 11;
        for (k, seat) in [to, from, to, from].into_iter().enumerate() {
            ev.push((last + 300 + 1_200 * k as u64, Ev::Out(at(seat))));
        }
        1_600
    } else {
        for (k, seat) in [from, to, from, to, from].into_iter().enumerate() {
            ev.push((300 + 1_200 * k as u64, Ev::Out(at(seat))));
        }
        3_000
    };
    (r.run(&ev, tail), ch)
}

/// A BODY FAR FROM THE CARET READS ITS OWN ROWS ([`OldPath::WorldReadsTheBodysRows`]): a pane
/// past the rows or the columns the console's map reads at once is read about its caret, and
/// about the body in a second window, so the head law reads the rows over and under a body
/// wherever the caret leaps. On the alternate screen between rows lettered to the margin
/// ([`FAR_LEAPS`]), the program moves the caret far from the body and back, twice: 40 and 58
/// rows down on a pane 100 rows tall from rest at the prompt's 58th column, 40 rows down after
/// the Backspaces at its 15th, five rows down to the 290th column on a pane 300 columns wide
/// after the Backspaces, 40 rows down from rest on the 85th row of a pane 160 rows tall, and 88
/// rows up from rest at the foot of a pane 100 rows tall. The body gathers to leap where it
/// rests fitted to its row, far from the caret. From 600 ms, the cat on dark and the dog on
/// Solarized Dark, keys typed at the window and, after the Backspaces, as the gallery types
/// them: no frame's drawn box meets a cell holding a glyph that the head law keeps it off
/// ([`kept_off`]), by a pixel or more, and none moves more than three rows from the frame
/// before (measured at most 1.9). On a blank page the same leaps find none, and no frame draws
/// the body down its row or fits it. Negative control ([`OldPath::WorldReadsTheBodysRows`]
/// reverted alone, the cat on dark): read about the caret alone, the head law lets the fit go as
/// the body gathers to leap, and its box meets those glyphs in each leap's floor of frames or
/// more (measured 38, 43, 9 and 11, 37 and 39, 38, and 10). In every leap the body follows the
/// caret away from the prompt's rows in 60 frames or more (91 to 308), and between the lettered
/// rows the head law holds it, drawn down or fitted, in 100 or more (153 to 374). The effects'
/// own `a_body_far_from_the_caret_reads_its_own_rows_on_every_face` judges these leaps and
/// others, and the control, on 13 x 29, 14 x 26 and 11 x 28 px cells.
#[test]
fn a_body_far_from_the_caret_reads_its_own_rows() {
    let from = 600;
    let mut runs = Vec::new();
    for leap in FAR_LEAPS {
        let drives: &[Drive] = if leap.2 {
            &[Drive::Console, Drive::Ctl]
        } else {
            &[Drive::Console]
        };
        for &drive in drives {
            for (style, look) in [("rainbow + cat", DARK), ("rainbow + dog", SOLARIZED_DARK)] {
                runs.push((leap, style, look, drive, true, false));
                runs.push((leap, style, look, drive, false, false));
            }
            runs.push((leap, "rainbow + cat", DARK, drive, true, true));
        }
    }
    let seen = aterm_spec::check::each(&runs, |&(leap, style, look, drive, lettered, control)| {
        let old: &[OldPath] = if control {
            &[OldPath::WorldReadsTheBodysRows]
        } else {
            &[]
        };
        let (judged, ch) = far_from_the_caret(style, look, (leap, lettered), old, drive);
        let prompt = i32::from(leap.3.0) - 1;
        let away = judged
            .iter()
            .filter(|j| {
                j.ms >= from
                    && j.coat
                        .pet
                        .is_some_and(|p| p.3 <= (prompt - 1) * ch || p.3 > (prompt + 2) * ch)
            })
            .count();
        let held = judged
            .iter()
            .filter(|j| {
                j.ms >= from
                    && j.coat
                        .sink_fit
                        .is_some_and(|(sink, fit)| sink > 0.0 || fit < 1.0)
            })
            .count();
        let step = judged
            .windows(2)
            .filter(|w| w[1].ms >= from)
            .filter_map(|w| Some((w[1].coat.pet?.2 - w[0].coat.pet?.2).abs()))
            .max()
            .unwrap_or(0);
        (away, held, kept_off(&judged, from, leap, ch), step, ch)
    });
    for (&(leap, style, look, drive, lettered, control), &(away, held, met, step, ch)) in
        runs.iter().zip(&seen)
    {
        let (name, look) = (leap.0, look.name);
        assert!(
            away >= 60,
            "fixture: {style} on {look} ({name}, {drive:?}) follows the caret ({away} frames)"
        );
        assert!(
            !lettered || held >= 100,
            "fixture: {style} on {look} ({name}, {drive:?}) is fitted under the lettered rows \
             ({held} frames)"
        );
        if control {
            let bites = leap.5[usize::from(drive == Drive::Ctl)];
            assert!(
                met >= bites,
                "negative control ({name}, {drive:?}): read about the caret alone, the box met \
                 the glyphs it is kept off in {met} frames"
            );
        } else {
            assert_eq!(
                (met, if lettered { 0 } else { held }),
                (0, 0),
                "{style} on {look} ({name}, {drive:?}, lettered {lettered}): frames whose box \
                 meets the glyphs it is kept off, frames drawn down or fitted on a blank page"
            );
            assert!(
                step <= 3 * ch,
                "{style} on {look} ({name}, {drive:?}, lettered {lettered}): a box moved {step} \
                 px in a frame"
            );
        }
    }
}

/// A FULL-SCREEN PROGRAM'S ROWS UNDER ITS LINE ARE ITS OWN ([`OldPath::FullScreenKeepsItsRow`]):
/// in the `steam` scene on the alternate screen ([`Steam::FullScreen`]) the cat keeps its line's
/// row. Seated three rows under the prompt at rest and while the hand wrote, it flew up across
/// the lettered row under the prompt at the first Backspace and back down across it once the
/// caret rested (under Linux's bundled JetBrains Mono, 13 x 29 px cells: 26 of the cat's frames
/// on dark whose box met that row's letters, 1,392 px of their ink under its body). Kept on its
/// row, it comes to rest whole flush past the row above's last letter, and sets off back into
/// the line drawn down or fitted for its way ([`OldPath::WayBeforeSettingOff`]): drawn for it
/// only from its first step or its take-off, its box met that letter's cell for three or four
/// frames as it went. Between the gallery's own rows and between descenders and a sentence, the
/// cat on dark and the dog on Solarized Dark, keys typed at the window and as the gallery types
/// them, from 600 ms to the scene's end: no frame's foot lies past the row under the prompt (a
/// body drawn down its row reaches into that row where it is blank,
/// [`aterm_effects::kitty_pet::head_law`]), none meets a lettered cell of that row by a pixel
/// or more or lays the body on its ink, and no frame's box meets a lettered cell of the row
/// above. Negative controls (each law reverted alone, the cat on dark, the gallery's rows, both
/// ways of typing): with [`OldPath::FullScreenKeepsItsRow`] 40 frames or more stand under the
/// row under and 10 or more meet its letters (167 and 26 at the window, 98 and 29 as the
/// gallery types, under JetBrains Mono; 98 and 31 under Iosevka Term's 11 x 28); with
/// [`OldPath::WayBeforeSettingOff`] two frames or more meet the row above's letters as it sets
/// off (three and four there). The effects' own tests judge both laws, and both controls, on
/// 13 x 29, 14 x 26 and 11 x 28 px cells.
#[test]
fn a_full_screen_program_s_rows_under_its_line_are_its_own() {
    let from = 600;
    // Frames whose foot lies past the row under the prompt (row 5), and frames whose box or
    // body meets that row's letters.
    let under = |judged: &[Judged], ch: i32| -> (usize, usize) {
        let past = judged
            .iter()
            .filter(|j| j.ms >= from && j.coat.pet.is_some_and(|p| p.3 > 6 * ch))
            .count();
        let on = judged
            .iter()
            .filter(|j| {
                j.ms >= from
                    && (j.coat.box_cells.iter().any(|c| c.0 == 5)
                        || j.coat.cells.iter().any(|c| c.0 == 5))
            })
            .count();
        (past, on)
    };
    let mut runs = Vec::new();
    for rows in STEAM_ROWS {
        for (style, look) in [("rainbow + cat", DARK), ("rainbow + dog", SOLARIZED_DARK)] {
            for drive in [Drive::Console, Drive::Ctl] {
                runs.push((rows, style, look, drive));
            }
        }
    }
    aterm_spec::check::each(&runs, |&(rows, style, look, drive)| {
        let page = steam_page(rows, Steam::FullScreen);
        let (judged, ch) = steamed_between_lettered_rows(style, look, &page, &[], drive);
        let drawn = judged
            .iter()
            .filter(|j| j.ms >= from && j.coat.pet.is_some())
            .count();
        assert!(
            drawn >= 200,
            "fixture: {style} on {} between {rows:?} ({drive:?}) is drawn ({drawn} frames)",
            look.name
        );
        assert_eq!(
            (under(&judged, ch), boxes_on_glyphs(&judged, from, 3..=3)),
            ((0, 0), 0),
            "{style} on {} between {rows:?} ({drive:?}): frames standing past the row under the \
             prompt, frames on that row's letters; frames whose box meets the row above's letters",
            look.name
        );
    });
    for drive in [Drive::Console, Drive::Ctl] {
        let page = steam_page(STEAM_ROWS[0], Steam::FullScreen);
        let (old, ch) = steamed_between_lettered_rows(
            "rainbow + cat",
            DARK,
            &page,
            &[OldPath::FullScreenKeepsItsRow],
            drive,
        );
        let (past, on) = under(&old, ch);
        assert!(
            past >= 40 && on >= 10,
            "negative control ({drive:?}): seated under the line, the cat stood past the row \
             under in {past} frames and met its letters in {on}"
        );
        let (old, _) = steamed_between_lettered_rows(
            "rainbow + cat",
            DARK,
            &page,
            &[OldPath::WayBeforeSettingOff],
            drive,
        );
        let above = boxes_on_glyphs(&old, from, 3..=3);
        assert!(
            above >= 2,
            "negative control ({drive:?}): drawn for its way from its first step or take-off, \
             the cat's box met the row above's letters in {above} frames"
        );
    }
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
/// fitted there, so the law is read at a line's end where the body would be held fitted: a
/// line typed on a prompt between a sentence whose letters end at its column 43 and three
/// lettered rows under (no seat under the box), on past the sentence's end
/// ([`fitted_then_past_the_line`]), where the cat walks fitted under the sentence by the caret
/// and comes to rest past its end whole (taller than 1.4 rows over the last 300 ms), its box on
/// no glyph's cell and its body on no ink from 600 ms but for the first keys' letters as it gets
/// going ([`past_the_first_keys`]; a seat reads its own row, [`OldPath::SeatReadsItsOwnRow`]:
/// reverted, the cat came there fitted, its box on the letters in 8 frames). Negative control
/// (the law reverted, the cat on dark, that prompt): held across the line's end, it rests
/// fitted (no taller than 1.05 rows).
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
        let (judged, ch) = fitted_then_past_the_line(old, 0);
        let moving = past_the_first_keys(&judged, 1_000, 4);
        let off = boxes_on_glyphs(&moving, 600, 0..=23) + sum(&moving, 600).on_ink;
        (last_heights(&judged, 300, ch), off)
    };
    let (rest, off) = held_first(&[]);
    assert!(
        off == 0 && !rest.is_empty() && rest.iter().all(|&h| h > 1.4),
        "past the sentence's end it rests whole ({rest:.2?} rows), off the letters ({off} \
         frames on a glyph's cell or on ink)"
    );
    let (rest, _) = held_first(&[OldPath::LineEndIsNoGap]);
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
    let old = context_unworded("rainbow + cat", DARK, true, &[OldPath::SeatReadsItsOwnRow]);
    let boxed = boxes_on_glyphs(&old, 600, under);
    assert!(
        boxed >= 10,
        "negative control: on the selected line's letters in {boxed} frames"
    );
}

/// A line typed on a prompt (row 4) between a sentence whose letters end at its column 43 and
/// three lettered rows under it (no seat under the box), on past the sentence's end
/// ([`PAST_THE_LINE_TEXT`]), and `back` letters deleted after it, to 2 s after the last key, the
/// companion's laws in `old` reverted, every frame's coat read; and the replay's cell height.
/// Under the sentence no seat by the caret keeps the body whole: the cat by the caret is fitted
/// to its row there (r29 K9-4: it waits by the caret, not at the seat past the sentence's end),
/// walks on with the writing past the sentence's end, drawn whole there, and comes back a few
/// cells with the deletes.
fn fitted_then_past_the_line(old: &[OldPath], back: u64) -> (Vec<Judged>, i32) {
    let below = "Below: every glyph here must stay legible! pqgyj {braces} [ok] and more";
    let mut r = Replay::new("rainbow + cat", DARK, TrailColor::Automatic);
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
    ev.extend(typed(PAST_THE_LINE_TEXT, 1_000, 60));
    let erase_from = 1_000 + 60 * PAST_THE_LINE_TEXT.len() as u64 + 300;
    ev.extend((0..back).map(|k| {
        (
            erase_from + 50 * k,
            Ev::Key('\u{7f}', b"\x08 \x08".to_vec()),
        )
    }));
    (r.run(&ev, 2_000), ch)
}

/// The line [`fitted_then_past_the_line`] types: from the prompt's column 2 it runs on past the
/// sentence's last letter (its column 43) to column 50.
const PAST_THE_LINE_TEXT: &str = "echo the quick brown fox jumps over the lazy dog";

/// A WALK'S LOOK AHEAD ENDS AT ITS STAND: the head law reads the row above ahead of a walking
/// body as far as its speed carries it in the law's ease ([`OldPath::HeadLooksAhead`]), so the
/// body is down or fitted by the time it reaches a word. A body walking a few cells back to a
/// whole seat beside a line's last letter read the letters past the seat, where it never goes:
/// it was fitted on the way and grew again at the seat, a dip and return (the companion that
/// blinks). The look ahead now stops at the stand the walk goes to
/// ([`OldPath::LookAheadStopsAtTheStand`]). In [`fitted_then_past_the_line`] with six letters
/// deleted back after the line (the caret back to its column 44, the stand by it beside the
/// sentence's last letter), once the cat has first stood whole on the prompt's row, no frame on
/// that row draws it fitted (no taller than 1.05 rows). Negative control (the law reverted):
/// frames on the walk back to the stand are fitted.
#[test]
fn a_walk_s_look_ahead_ends_at_its_stand() {
    let fitted_after_whole = |old: &[OldPath]| -> (usize, usize) {
        let (judged, ch) = fitted_then_past_the_line(old, 6);
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
    let (_, fitted) = fitted_after_whole(&[OldPath::LookAheadStopsAtTheStand]);
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
        "flat rainbow + cat",
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
/// the cell height, the frames from the one it is down on to the scene's end whose drawn box
/// meets a glyph's cell on the first row or whose body lies on that row's ink, and those frames
/// from the wrap on, its crossing among them.
fn wrapped_coat(style: &str, look: Look, old: &[OldPath]) -> (i32, i32, usize, usize) {
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
    let meets_the_line = |j: &&crate::trail_letters_read::Judged| {
        j.coat.box_cells.iter().any(|c| c.0 == 4) || j.coat.cells.iter().any(|c| c.0 == 4)
    };
    let on_the_line = judged
        .iter()
        .filter(|j| j.ms >= down)
        .filter(meets_the_line)
        .count();
    let crossing = judged
        .iter()
        .filter(|j| j.ms >= wrap)
        .filter(meets_the_line)
        .count();
    (apex, ch, on_the_line, crossing)
}

/// A COMPANION CROSSING A WRAP AND RESTING BESIDE THE CARET ON THE LINE'S SECOND ROW KEEPS OFF
/// THE LINE (r24 K7-4, K7-6): a line typed past the margin onto a second row and stopped there
/// ([`Scene::Wrapped`]). The body has slid under the line at the margin
/// ([`OldPath::MarginKeepsTheLine`]), so from the wrap on, through its crossing along the second
/// row, its run with the writing and its rest beside the caret under the line's full first row,
/// no frame's drawn box meets a glyph's cell on the first row or lays the body on its ink: its
/// head is drawn down its row ([`aterm_effects::kitty_pet::head_law`]). Every companion style, on
/// dark, Solarized Dark, GitHub Light and Default Light. Negative controls: with the margin's
/// slide reverted alone the crossing leaves from the first row, and meets its letters; there the
/// wrap's crossing still carries the body high over the first row, its box's foot at the bound's
/// apex no more than a quarter row into it, the landing's head law judged and eased in for the
/// crossing's last `HEAD_EASE_S` only ([`OldPath::SinkAtTheLanding`], reverted with the slide, the
/// foot stays three quarters of a row into it, the wall on the line held as it was,
/// [`OldPath::CaretCellIsNoSeat`] reverted with them); without the head law
/// ([`OldPath::HeadUnderLetters`]) its head lies on the first row's letters beside the caret.
#[test]
fn a_companion_crossing_a_wrap_keeps_off_the_line() {
    for (style, look) in
        COMPANIONS
            .into_iter()
            .zip([DARK, SOLARIZED_DARK, GITHUB_LIGHT, DEFAULT_LIGHT])
    {
        let (.., crossing) = wrapped_coat(style, look, &[]);
        assert_eq!(
            crossing, 0,
            "{style} on {}: frames from the wrap on whose box meets the first row's letters or \
             whose body lies on their ink",
            look.name
        );
        let (apex, ch, on_the_line, _) = wrapped_coat(style, look, &[OldPath::MarginKeepsTheLine]);
        assert!(
            apex <= 4 * ch + ch / 4,
            "{style} on {}, no slide: the crossing's apex left its box's foot {:.2} rows into \
             the line's first row",
            look.name,
            f64::from(apex - 4 * ch) / f64::from(ch)
        );
        assert_eq!(
            on_the_line, 0,
            "{style} on {}, no slide: frames down on the second row whose box meets the first \
             row's letters or whose body lies on their ink",
            look.name
        );
    }
    let (.., through) = wrapped_coat("rainbow + cat", DARK, &[OldPath::MarginKeepsTheLine]);
    assert!(
        through > 0,
        "negative control: without the margin's slide the crossing kept off the first row"
    );
    // The wall on the line held as it was ([`OldPath::CaretCellIsNoSeat`] reverted with the
    // slide): kept, the caret coming into the body at the wall sends it off the line, and the
    // crossing never leaves from the first row.
    let (low, ch, ..) = wrapped_coat(
        "rainbow + cat",
        DARK,
        &[
            OldPath::MarginKeepsTheLine,
            OldPath::SinkAtTheLanding,
            OldPath::CaretCellIsNoSeat,
        ],
    );
    assert!(
        low > 4 * ch + ch / 4,
        "negative control: with the landing's sink from the take-off the apex still cleared \
         the line ({:.2} rows into it)",
        f64::from(low - 4 * ch) / f64::from(ch)
    );
    let (.., headed) = wrapped_coat("rainbow + cat", DARK, &[OldPath::HeadUnderLetters]);
    assert!(
        headed > 0,
        "negative control: without the head law the companion kept its head off the first row"
    );
}

/// ON A FULL SCREEN'S LAST ROW THE COMPANION STANDS BESIDE THE CARET (the trail gallery's
/// `last-row`): the row over it holds a letter in every cell and no row under it is on the grid,
/// so it has no seat off the line and is fitted to its row. Typed along that row to the margin and
/// held there, past the first keys, no frame's drawn box meets a lettered cell, and it never leaves
/// the glass. Every companion style on every page. The line ends as near the grid's end as leaves
/// the body its room as it draws it there: the fitted body's ([`last_row_cols`]), the gallery's
/// three cells on its 14 x 26 px cell, four under Linux's 13 x 29, where the body is 3.38 cells
/// across, or, where the widest box the cat on dark draws at rest in that room takes more, as many
/// cells as that box takes ([`Scene::LastRowRoom`]). The grid's end pushes a pose's swell there back
/// toward the line: under Windows' Iosevka Term NL at 22 px (11 x 28) the fitted body is 3.86
/// cells across and four cells seat it, but as it sits, its fold and its purr's breath widen its
/// box to 45 px of their 44, and it lay 1 px into the line's last letter's cell in 3 frames, on
/// none of its ink, the reach the wall's law bounds ([`SWELL`]); there the line leaves five
/// (Windows x86_64, 2026-11-12). A line that ends nearer the
/// grid's end than the fitted body's room leaves the body no seat, and the wall's law hides it
/// there instead ([`a_last_row_that_holds_no_seat_hides_the_companion`], which judges the swell
/// at the fitted body's room on 11 x 28 too). Negative controls, the cat on dark:
/// [`OldPath::FitCentredAtTheWall`] reverted alone, its box lies across the line's last letter
/// from the line's last key, and the wall, which now holds it no seat, hides it, off the glass in
/// 250 or more of the 565 frames held (measured 375, its box on the letter in 3 before the wall's
/// cut took it off); with [`OldPath::LettersCutTheFade`] reverted too, its fade lies on the
/// letter in 19 or more (measured 28, off the glass in 350); with it and
/// [`OldPath::NoSeatHidesTheBody`] reverted (not taken), it meets the letter in 378 and never
/// leaves the glass (Linux arm64, 2026-11-09; the first two the same at five cells on 11 x 28,
/// Windows x86_64, 2026-11-12). Each floor is two thirds of the measure, rounded up.
#[test]
fn on_a_full_screen_s_last_row_the_companion_stands_beside_the_caret() {
    const FIRST_KEYS: u64 = 300;
    let (ev, _) = Scene::LastRow.events();
    let enter = ev
        .iter()
        .find(|(_, e)| matches!(e, crate::trail_letters_read::Ev::Key('\r', _)))
        .map_or(0, |(at, _)| *at);
    // The room past the line: the fitted body's, or the cells its widest box at rest there takes.
    let seated = last_row_cols(gallery_cell()) - last_row_line();
    let widest = coats_driven("rainbow + cat", DARK, Scene::LastRow, &[], Drive::Ctl)
        .iter()
        .filter(|j| j.ms >= TYPING_FROM + FIRST_KEYS && j.ms < enter)
        .filter_map(|j| j.coat.pet)
        .filter_map(|(x0, x1, ..)| usize::try_from(x1 - x0).ok())
        .max()
        .expect("fixture: the companion is on glass beside the caret");
    let taken = u16::try_from(widest.div_ceil(gallery_cell().0)).expect("a room");
    let room = i16::try_from(seated.max(taken)).expect("a room");
    let scene = Scene::LastRowRoom(room, LastRowThen::Rests);
    type Held = (u64, Vec<(usize, usize)>, bool);
    let held = |style: &str, look: Look, old: &[OldPath]| -> Vec<Held> {
        coats_driven(style, look, scene, old, Drive::Ctl)
            .iter()
            .filter(|j| j.ms >= TYPING_FROM + FIRST_KEYS && j.ms < enter)
            .map(|j| (j.ms, j.coat.box_cells.clone(), j.coat.pet.is_some()))
            .collect()
    };
    assert_eq!(
        last_row_cols((14, 26)),
        COLS,
        "fixture: on the gallery's own 14 x 26 px cell the scene is the gallery window's grid"
    );
    for look in [DARK, SOLARIZED_DARK, GITHUB_LIGHT, DEFAULT_LIGHT] {
        for style in COMPANIONS {
            let frames = held(style, look, &[]);
            let met: Vec<_> = frames.iter().filter(|f| !f.1.is_empty()).collect();
            assert!(
                met.is_empty(),
                "{style} on {}, {room} cells past the line: {} frames on the letters: {met:?}",
                look.name,
                met.len()
            );
            assert!(
                frames.iter().all(|f| f.2),
                "{style} on {}: the companion left the glass",
                look.name
            );
        }
    }
    let old = held("rainbow + cat", DARK, &[OldPath::FitCentredAtTheWall]);
    assert!(
        old.iter().filter(|f| !f.2).count() >= 250,
        "negative control: centred in its whole box the fitted body kept its seat at the wall"
    );
    let old = held(
        "rainbow + cat",
        DARK,
        &[OldPath::FitCentredAtTheWall, OldPath::LettersCutTheFade],
    );
    assert!(
        old.iter().filter(|f| !f.1.is_empty()).count() >= 19,
        "negative control: centred in its whole box the fitted body kept off the last letter"
    );
}

/// What a [`Scene::LastRowRoom`] replay shows the wall's law, from 300 ms past the first key.
#[derive(Debug, Default)]
struct WallCoat {
    /// Frames to the Enter (wrapped: to the wrap), and those on glass.
    held: usize,
    drawn: usize,
    /// Of those on glass: frames whose box meets a lettered cell, those whose box reaches more
    /// than [`SWELL`] into one, those whose body lies on letter ink, and those whose body lies in
    /// the caret's cell.
    boxed: usize,
    deep: usize,
    inked: usize,
    careted: usize,
    /// The first frame (ms) on glass whose box meets a lettered cell.
    first: Option<u64>,
    /// The first frame (ms) from `first` off the glass.
    gone: Option<u64>,
    /// Frames (ms) from `first` to `gone` whose alpha rose over the frame before's.
    rose: Vec<u64>,
    /// Frames from `first` to `gone` on glass below full alpha whose box meets a lettered cell or
    /// whose body lies on letter ink or the caret's cell: the wall's fade over the letters.
    faded_on: usize,
    /// Frames (ms) on glass from `gone` to the Enter (not judged where the line wraps).
    unfaded: Vec<u64>,
    /// The first frame (ms) on glass from `gone` to the Enter: the companion back before it.
    returned: Option<u64>,
    /// Frames on glass from `gone` to the Enter whose box reaches more than [`SWELL`] into a
    /// lettered cell, or whose body lies on letter ink or the caret.
    relaid: Vec<String>,
    /// Frames on glass from the first key past the burst's line ([`last_row_times`]) to the Enter
    /// whose box reaches more than [`SWELL`] into a lettered cell, or whose body lies on letter
    /// ink or the caret.
    then_on: usize,
    /// Of those, the frames at full alpha.
    full: usize,
    /// Frames on glass past the Enter, to the next line's first key (wrapped: past the wrap, to
    /// the Enter) whose box reaches more than [`SWELL`] into a lettered cell, or whose body lies
    /// on letter ink or the caret: when (ms), and what.
    after: Vec<(u64, String)>,
    /// The first frame (ms) on glass past the Enter (wrapped: the wrap) and past `gone`.
    back: Option<u64>,
    /// The burst's last key, and the Enter (wrapped: the wrap), ms.
    last_key: u64,
    until: u64,
}

/// How far across into a lettered cell a pose's swell at the wall reaches where the line seats
/// the companion, px ([`Coat::box_reach`]), which the wall's law leaves: the grid's end pushes
/// the swell back onto the line (`OldPath::StretchOffLetters` leaves it so). The effects replay's
/// bound, read as it reads it, across alone
/// (`steam_tests::a_wall_that_holds_no_seat_hides_the_body_on_every_face`); measured 1 px on
/// 11 x 28 where the line leaves four blank cells, none on 13 x 29 or 14 x 26 (Linux arm64,
/// 2026-11-08).
const SWELL: usize = 2;

/// A [`Scene::LastRowRoom`] replay's [`WallCoat`]: `wraps`, the line wraps at its last key.
fn wall_coat(judged: &[Judged], (room, then): (i16, LastRowThen)) -> WallCoat {
    let wraps = room < 0;
    let (ev, _) = Scene::LastRowRoom(room, then).events();
    let (last_key, then_from, enter) = last_row_times(then);
    let next_line = ev
        .iter()
        .filter(|(_, e)| matches!(e, Ev::Key(c, _) if *c != '\r'))
        .map(|(at, _)| *at)
        .find(|&k| k > enter)
        .unwrap_or(u64::MAX);
    let (until, after_until) = if wraps {
        (last_key, enter)
    } else {
        (enter, next_line)
    };
    let on = |c: &Coat| c.alpha > 0 && (c.box_reach > SWELL || c.ink_px > 0 || c.caret_px > 0);
    let told = |j: &&Judged| {
        format!(
            "{} ms, box {:?}: {:?} {} px in, ink {} caret {}",
            j.ms, j.coat.pet, j.coat.box_cells, j.coat.box_reach, j.coat.ink_px, j.coat.caret_px
        )
    };
    let held: Vec<&Judged> = judged
        .iter()
        .filter(|j| j.ms >= TYPING_FROM + 300 && j.ms < until)
        .collect();
    let first = held
        .iter()
        .find(|j| j.coat.alpha > 0 && !j.coat.box_cells.is_empty())
        .map(|j| j.ms);
    let gone = first.and_then(|first| {
        judged
            .iter()
            .find(|j| j.ms >= first && j.coat.alpha == 0)
            .map(|j| j.ms)
    });
    let mut w = WallCoat {
        held: held.len(),
        drawn: held.iter().filter(|j| j.coat.alpha > 0).count(),
        boxed: held
            .iter()
            .filter(|j| j.coat.alpha > 0 && !j.coat.box_cells.is_empty())
            .count(),
        deep: held
            .iter()
            .filter(|j| j.coat.alpha > 0 && j.coat.box_reach > SWELL)
            .count(),
        inked: held
            .iter()
            .filter(|j| j.coat.alpha > 0 && j.coat.ink_px > 0)
            .count(),
        careted: held
            .iter()
            .filter(|j| j.coat.alpha > 0 && j.coat.caret_px > 0)
            .count(),
        first,
        gone,
        then_on: then_from.map_or(0, |t| {
            held.iter().filter(|j| j.ms >= t && on(&j.coat)).count()
        }),
        full: then_from.map_or(0, |t| {
            held.iter()
                .filter(|j| j.ms >= t && on(&j.coat) && j.coat.alpha == u8::MAX)
                .count()
        }),
        last_key,
        until,
        ..WallCoat::default()
    };
    if let (Some(first), Some(gone)) = (first, gone) {
        w.rose = judged
            .windows(2)
            .filter(|p| p[0].ms >= first && p[1].ms <= gone && p[1].coat.alpha > p[0].coat.alpha)
            .map(|p| p[1].ms)
            .collect();
        w.faded_on = judged
            .iter()
            .filter(|j| {
                let c = &j.coat;
                j.ms >= first
                    && j.ms < gone
                    && c.alpha > 0
                    && c.alpha < u8::MAX
                    && (!c.box_cells.is_empty() || c.ink_px > 0 || c.caret_px > 0)
            })
            .count();
        if !wraps {
            let rested: Vec<&&Judged> = held.iter().filter(|j| j.ms >= gone).collect();
            w.unfaded = rested
                .iter()
                .filter(|j| j.coat.alpha > 0)
                .map(|j| j.ms)
                .collect();
            w.returned = w.unfaded.first().copied();
            w.relaid = rested
                .iter()
                .filter(|j| on(&j.coat))
                .map(|j| told(j))
                .collect();
        }
    }
    let past: Vec<&Judged> = judged
        .iter()
        .filter(|j| j.ms > until && j.ms < after_until)
        .collect();
    w.after = past
        .iter()
        .filter(|j| on(&j.coat))
        .map(|j| (j.ms, told(j)))
        .collect();
    w.back = past
        .iter()
        .find(|j| j.coat.alpha > 0 && gone.is_none_or(|g| j.ms > g))
        .map(|j| j.ms);
    w
}

/// The blank cells past the burst's line on a grid `room` cells wider than it once `keys`
/// Backspaces have erased it ([`LastRowThen::Erased`]): a cell each, and two for the first where
/// the line ran to the grid's end, since the caret stood on its last letter there, the back-step
/// leaves that letter after it, and the erase to the line's end takes it.
fn erased_room(room: i16, keys: u16) -> i16 {
    room + keys as i16 + i16::from(room == 0 && keys > 0)
}

/// What [`a_last_row_that_holds_no_seat_hides_the_companion`] replays past the burst's line where
/// the line leaves the companion no seat: the rest, an erase after the fade (3, 6 and 12
/// Backspaces), one during it, Left arrows back into the line, a pause before its last keys
/// (replayed where the line seats the companion too, on the bundled face's cell), the row over
/// the line cleared, and a line whose last letters follow a run of spaces (replayed where it seats
/// the companion too, on the bundled face's cell).
const WALL_THENS: [LastRowThen; 9] = [
    LastRowThen::Rests,
    LastRowThen::Erased {
        keys: 3,
        after: 1_500,
    },
    LastRowThen::Erased {
        keys: 6,
        after: 1_500,
    },
    LastRowThen::Erased {
        keys: 12,
        after: 1_500,
    },
    LastRowThen::Erased {
        keys: 6,
        after: 150,
    },
    LastRowThen::Back,
    LastRowThen::Paused,
    LastRowThen::Opened,
    LastRowThen::Gapped,
];

/// The faces [`a_last_row_that_holds_no_seat_hides_the_companion`] draws the wall's scene in
/// besides the window's, each where this host resolves it: Liberation Mono at 23 px, which lays
/// the gallery's 14 x 26 px cell (a face Linux hosts install, which aterm does not bundle), and
/// Iosevka Term at 22 px, 11 x 28 (bundled with aterm-render's `embedded-font`).
const WALL_FACES: [(&str, f32); 2] = [
    ("Liberation Mono", 23.0),
    (aterm_render::bundled::IOSEVKA_FAMILY, 22.0),
];

/// A face the wall's scene is drawn in: its family, the face this host resolved it to and its px
/// (`None`: the window's face at the gallery's px), and the cell it lays, px.
struct WallFace {
    family: &'static str,
    face: Option<(FaceRef, f32)>,
    cell: (usize, usize),
}

/// The window's face, then each of [`WALL_FACES`] this host resolves to a face that loads as
/// itself (not the window's face standing in for it).
fn wall_faces() -> Vec<WallFace> {
    let mut faces = vec![WallFace {
        family: "the window's face",
        face: None,
        cell: gallery_cell(),
    }];
    for (family, px) in WALL_FACES {
        let Some(face) = aterm_render::resolve_family_or_bundled(family, FaceTarget::REGULAR)
        else {
            continue;
        };
        let Some(rend) = Renderer::from_system_with_face(Some(&face), px, Theme::default()) else {
            continue;
        };
        if rend.primary_source_path() == Some(face.path.as_str()) {
            faces.push(WallFace {
                family,
                cell: rend.cell_size(),
                face: Some((face, px)),
            });
        }
    }
    faces
}

/// A run of the wall's scene: the face ([`wall_faces`]'s index), the companion, the page, the
/// blank cells past the line and what follows the burst, how the keys come, and the law its
/// negative control reverts, if it is one.
type WallRun = (
    usize,
    &'static str,
    Look,
    (i16, LastRowThen),
    Drive,
    Option<OldPath>,
);

/// THE WALL THAT HOLDS NO SEAT HIDES THE COMPANION ([`OldPath::NoSeatHidesTheBody`]; the trail
/// gallery's `last-row`, [`Scene::LastRowRoom`]): on a full page's last row the row over the line
/// holds a letter in every cell and none lies under it, so the companion has no seat off the line
/// and is fitted to its row at the wall. The burst's line ends 4, 3, 2, 1 and 0 cells short of the
/// grid's end, and one cell past it, so that it wraps and the page scrolls. Drawn in the window's
/// face at the gallery's px, every companion style on every page, and in each of [`WALL_FACES`]
/// this host resolves (Liberation Mono at 23 px on 14 x 26, Iosevka Term at 22 px on 11 x 28), the
/// cat and the dog on dark; keys typed past the console (as the gallery types them) and through
/// it; from 300 ms past the first key. Where the line leaves the fitted body the room it takes on
/// the face's cell (`last_row_cols`: four cells on 13 x 29 and 11 x 28, three on 14 x 26), typed
/// to its end at once (and, in each face but a window's face that does not lay the bundled
/// 13 x 29, paused six keys short of it, its last letters after a run of spaces, walked back into
/// by ten Left arrows, which leave the companion at its seat off the line,
/// [`OldPath::KeptOffTheLine`], or the row over it cleared, [`OldPath::FitWaitsForTheFeet`]), it
/// is on glass in every frame to the Enter, and none lays its box more than [`SWELL`] across into
/// a lettered cell (a pose's swell at the wall: 1 px on 11 x 28, in 3 to 16 frames; none on
/// 13 x 29 and 14 x 26) or its body on letter ink or the caret's cell. Negative control of that
/// reach (built to measure, not kept): with [`head_law`]'s fit raised to 1.06 rows, the box
/// 1 to 2 px down into the lettered row over the line, it reached across it in 486 of the 565
/// frames at four blank cells on 13 x 29 (Linux arm64, 2026-11-09). Where it leaves less, a frame's
/// box meets the last letters as they come, and from that frame the companion fades out (0.45 s),
/// drawn off the glass within the wall's cut of it ([`OldPath::LettersCutTheFade`], 64 ms): its
/// alpha never rises, it lies on the letters or the caret below full alpha in 3 frames at most
/// (the cut's 4 frames at 60 Hz, the 4th off the glass; measured 3 in every run, at alphas 184,
/// 118 and 57), and it is off the glass the cut and a frame on (measured 48 ms, and 64 where the
/// first frame on the letters comes at full alpha, a tick before the wall's law hides it, past the
/// pause). Past the Enter (to the next line's first key), and past the wrap (to the Enter), it is
/// back on glass within a second, and no frame lays its box more than [`SWELL`] into a lettered
/// cell or its body on ink or the caret: it comes back by the new caret, and the wrapped line's
/// first row stays clear. In those 3 frames (the cat and the dog on dark), at rest, past the Left
/// arrows, the erases and the cleared row, its art lies on no ink and off the caret's cell on
/// 13 x 29 and 11 x 28, and on 14 x 26 on 2 to 7 px of ink at two and one blank cells, on none at
/// none or where the line wraps. Past the pause the first of its 4 frames on the letters is at
/// full alpha, on no ink, and the last, at 57, lies on 37 to 51 px of ink at one blank cell on
/// 13 x 29 and 11 x 28 and at none on 14 x 26. Past the gap all 3 lie on ink at one blank cell and
/// at none (38 to 68 px on 13 x 29, 48 to 55 on 11 x 28, 2 to 63 on 14 x 26, and 6 to 9 at two
/// there). Without the cut the fade lay over the letters for its 28 frames, its art on up to 160
/// px of ink and the caret's cell in 19 at none on 13 x 29 (142 and 22 on 14 x 26, 146 and 19 on
/// 11 x 28; Linux arm64, 2026-11-09).
///
/// Where the line leaves no seat, what follows it before the Enter ([`WALL_THENS`]):
/// - It rests: off the glass from its fade's end to the Enter.
/// - 3, 6 or 12 Backspaces 1.5 s past the last key, or 6 from 150 ms, while its fade runs on (off
///   the glass by then): where the erase leaves the fitted body its room ([`erased_room`]), it is
///   back on glass within a second of the erase's last key (or of its fade's end), and where it
///   leaves less, off the glass to the Enter. Either way no frame from its fade's end to the Enter
///   lays its box more than [`SWELL`] into a lettered cell or its body on ink or the caret.
/// - Ten Left arrows back into the line 1.5 s past the last key: off the glass from its fade's
///   end to the Enter.
/// - The burst paused six keys short of its end for 3 s: off the glass from its fade's end to the
///   Enter.
/// - 1.5 s past the last key the program clears the row over the line
///   ([`LastRowThen::Opened`]), so a seat opens beside the line while the companion is out: back
///   on glass within a second of the clearing (or of its fade's end; measured 24 ms past the
///   clearing), and no frame from its fade's end to the Enter lays its box more than [`SWELL`]
///   into a lettered cell or its body on ink or the caret. Past the Enter, the frame before it moves has the Enter's
///   scroll carry the line up under that seat (the same at four blank cells, which seat it, and
///   with the law reverted): that frame is the scroll's, and is not judged.
/// - The burst's last two letters typed after fourteen spaces ([`LastRowThen::Gapped`]), so that
///   a stand by the caret off the letters lies in the spaces: off the glass from its fade's end
///   to the Enter (stood at that stand, it blinked back and out every 64 ms to the Enter in the
///   effects replay).
///
/// The pause, the gap, the Left arrows and the row over the line cleared are replayed where the
/// line seats the companion too, in each face but a window's face that does not lay the bundled
/// 13 x 29 (macOS's SF Mono on 14 x 26, where none of it was measured): on glass in every frame
/// to the Enter, and on no letter (but the swell, on 11 x 28) or the caret; the cleared row's seat
/// takes it off the line, grown only once it has risen to that row,
/// [`OldPath::FitWaitsForTheFeet`]. On 14 x 26 the effects replay's box overlaps the caret's cell
/// by 12 px at three blank cells, a seat; drawn on that cell in Liberation Mono, the companion's
/// art lies in the caret's cell in no frame there (Linux arm64, 2026-11-08).
///
/// In each, from the first key past the line (past the pause) to the Enter, at most one frame at
/// full alpha lays its box more than [`SWELL`] into a lettered cell or its body on ink or the caret
/// (measured: one past the pause at one blank cell and at none on 13 x 29 and 11 x 28, and at none
/// on 14 x 26; none in the others).
///
/// Negative control (the law reverted with [`OldPath::CaretCellIsNoSeat`], the cat on dark, both
/// ways of typing, every room that leaves no seat, in the window's face): it stands at the wall on the line's last letters to the Enter,
/// its box on them in 252 frames or more of the 565 held (two thirds of the fewest; measured 378 at
/// three cells, its art on ink in none, 381 and 325 at two, 384 and 381 at one, 387 and 384 at
/// none, the caret's cell in 378; past the gap, 378 at three and 381 at two, one and none), and
/// where the line wraps, typed past the console, its crossing back lays its box more than
/// [`SWELL`] into the wrapped line's first row in 21 frames or more (two thirds of the 31
/// measured). In each other face, at rest at every room that leaves no seat but the wrap, its box
/// on them in 252 or more as well (measured 378 to 384 on 14 x 26, 378 to 387 on 11 x 28). From the
/// first Left arrow to the Enter it follows the caret along the line's letters, on them in 190
/// frames or more (measured 284 of 284); from the end of the pause, in 252 or more (378 to 388 of
/// 394); and an erase after the line ran to the grid's end leaves it on them from the first
/// Backspace to the Enter in 22 or more (measured 284 with three, 283 with six, 33 with twelve, 366
/// with six while it fades; at the other rooms the reverted body is clear of them within a few
/// frames of the erase, and no control is taken there). The caret's cell is no seat either
/// ([`at_the_wall_the_caret_s_cell_is_no_seat`]): with the hide reverted alone, that law takes a body
/// whose box at the wall lies on the caret's cell off to a seat off the line once the hand's rhythm
/// closes, and the box met the letters only through that wait (54 frames at two blank cells on
/// 14 x 26, typed past the console, macOS, 2026-11-10), so the control reverts both: it is what
/// the wall did before either law. Negative control of the seat kept off the
/// line ([`OldPath::KeptOffTheLine`] reverted, the cat on dark, both ways of typing, the Left arrows
/// at four blank cells, which seat it): it follows the caret onto the line's letters, more than
/// [`SWELL`] into them from the first arrow to the Enter in 186 frames or more (measured 278 of 284,
/// 263 at full alpha, its art on their ink in 227; with the law, none; Linux arm64, 2026-11-08).
/// Negative control of the fit that waits for the feet ([`OldPath::FitWaitsForTheFeet`]
/// reverted, the cat on dark, both ways of typing, the row over the line cleared at four blank
/// cells): it grows as it rises off the line's row, and its box reaches more than [`SWELL`] into
/// the line's last letters in 4 frames or more from the clearing (measured 6, at full alpha, its
/// art on their ink in none; with the law, none; Linux arm64, 2026-11-08). Negative control of
/// the caret's own steps ([`OldPath::CaretTypesOn`] reverted, the cat on dark, keys typed past
/// the console, the pause at every room but the wrap):
/// the keys typed on after the pause find the margin untaken, and it walks over the caret onto the
/// line's last letters at full alpha, more than [`SWELL`] into them in 15 frames or more over the
/// rooms (measured 22: 10 at three blank cells and 10 at two, on up to 120 and 95 px of their ink,
/// 1 at one and 1 at none; 2026-11-08), against 2 with the law. Each floor is two thirds of the
/// fewest measured. Linux arm64, 2026-11-07. Negative control of the cut
/// ([`OldPath::LettersCutTheFade`] reverted, the cat on dark, both ways of typing, the rest at
/// every room that leaves no seat, in every face): its fade lies on the letters or the caret below
/// full alpha in 19 frames or more (measured 28 at every room that holds the line; to the wrap 12
/// on 13 x 29 and 11 x 28 and 9 on 14 x 26, so 6 or more there; Linux arm64, 2026-11-09).
#[test]
fn a_last_row_that_holds_no_seat_hides_the_companion() {
    const ROOMS: [i16; 6] = [4, 3, 2, 1, 0, -1];
    const PAGES: [Look; 4] = [DARK, SOLARIZED_DARK, GITHUB_LIGHT, DEFAULT_LIGHT];
    let cat = "rainbow + cat";
    let faces = wall_faces();
    let seated_at = |cell| i32::from(last_row_cols(cell)) - i32::from(last_row_line());
    let mut runs: Vec<WallRun> = Vec::new();
    for (f, face) in faces.iter().enumerate() {
        let window = face.face.is_none();
        let seated = seated_at(face.cell);
        // The window's face draws every companion on every page, another face the cat and the
        // dog on dark; each replays what follows a line that seats the companion where it was
        // measured (the window's face on the bundled 13 x 29 alone).
        let (styles, looks): (&[&str], &[Look]) = if window {
            (&COMPANIONS, &PAGES)
        } else {
            (&[cat, "rainbow + dog"], &[DARK])
        };
        let seats_judged = !window || lays_the_bundled_cell();
        for room in ROOMS {
            for then in WALL_THENS {
                let unseated = i32::from(room) < seated;
                let judged = match then {
                    LastRowThen::Rests => true,
                    LastRowThen::Paused
                    | LastRowThen::Gapped
                    | LastRowThen::Back
                    | LastRowThen::Opened => room >= 0 && (unseated || seats_judged),
                    LastRowThen::Erased { .. } => room >= 0 && unseated,
                };
                if !judged {
                    continue;
                }
                for drive in [Drive::Ctl, Drive::Console] {
                    for &look in looks {
                        for &style in styles {
                            runs.push((f, style, look, (room, then), drive, None));
                        }
                    }
                    if unseated && then == LastRowThen::Rests {
                        let old = Some(OldPath::LettersCutTheFade);
                        runs.push((f, cat, DARK, (room, then), drive, old));
                    }
                    if !window {
                        if unseated && room >= 0 && then == LastRowThen::Rests {
                            let old = Some(OldPath::NoSeatHidesTheBody);
                            runs.push((f, cat, DARK, (room, then), drive, old));
                        }
                        continue;
                    }
                    let control = match then {
                        LastRowThen::Rests => room >= 0 || drive == Drive::Ctl,
                        LastRowThen::Erased { .. } => room == 0,
                        LastRowThen::Back | LastRowThen::Paused | LastRowThen::Gapped => true,
                        LastRowThen::Opened => false,
                    };
                    if unseated && control {
                        let old = Some(OldPath::NoSeatHidesTheBody);
                        runs.push((f, cat, DARK, (room, then), drive, old));
                    }
                    if !unseated && then == LastRowThen::Back {
                        let old = Some(OldPath::KeptOffTheLine);
                        runs.push((f, cat, DARK, (room, then), drive, old));
                    }
                    if !unseated && then == LastRowThen::Opened {
                        let old = Some(OldPath::FitWaitsForTheFeet);
                        runs.push((f, cat, DARK, (room, then), drive, old));
                    }
                    if then == LastRowThen::Paused && drive == Drive::Ctl {
                        let old = Some(OldPath::CaretTypesOn);
                        runs.push((f, cat, DARK, (room, then), drive, old));
                    }
                }
            }
        }
    }
    let seen = aterm_spec::check::each(&runs, |&(f, style, look, scene, drive, control)| {
        // The hide's control reverts the caret's cell's law with it (the doc above).
        let old: Vec<OldPath> = match control {
            Some(OldPath::NoSeatHidesTheBody) => {
                vec![OldPath::NoSeatHidesTheBody, OldPath::CaretCellIsNoSeat]
            }
            _ => control.into_iter().collect(),
        };
        let face = faces[f].face.as_ref().map(|(face, px)| (face, *px));
        wall_coat(
            &coats_driven_on(
                face,
                style,
                look,
                Scene::LastRowRoom(scene.0, scene.1),
                &old,
                drive,
            ),
            scene,
        )
    });
    let full: Vec<usize> = runs
        .iter()
        .zip(&seen)
        .filter(|(run, _)| run.5 == Some(OldPath::CaretTypesOn))
        .map(|(_, w)| w.full)
        .collect();
    assert!(
        full.iter().sum::<usize>() >= 15,
        "negative control ({:?}): typed on past the console after the pause, it lay on the \
         letters at full alpha in only {full:?} frames",
        OldPath::CaretTypesOn
    );
    let (_, fit) = head_law(true, true);
    for (&(f, style, look, (room, then), drive, control), w) in runs.iter().zip(&seen) {
        let WallFace { family, cell, .. } = faces[f];
        let seated = seated_at(cell);
        let why = format!(
            "{style} on {}, {family} ({cell:?}), {room} cells past the line, {then:?} ({drive:?}; \
             body {:.2} cells)",
            look.name,
            art_cols(cell.0 as u16, cell.1 as u16) * fit
        );
        assert!(w.held >= 150, "fixture: {why} holds {} frames", w.held);
        if control == Some(OldPath::CaretTypesOn) {
            continue;
        }
        if control == Some(OldPath::LettersCutTheFade) {
            let bites = if room < 0 { 6 } else { 19 };
            assert!(
                w.faded_on >= bites,
                "negative control, {why}: the cut reverted, its fade lay on the letters in only \
                 {} frames ({w:?})",
                w.faded_on
            );
            continue;
        }
        if control == Some(OldPath::KeptOffTheLine) {
            assert!(
                w.then_on >= 186,
                "negative control, {why}: seated, from the first Left arrow it lay on the \
                 letters in only {} frames ({w:?})",
                w.then_on
            );
            continue;
        }
        if control == Some(OldPath::FitWaitsForTheFeet) {
            assert!(
                w.then_on >= 4,
                "negative control, {why}: seated, from the clearing over the line its box met \
                 the letters in only {} frames ({w:?})",
                w.then_on
            );
            continue;
        }
        if control.is_some() {
            let (met, bites) = match then {
                LastRowThen::Rests if room < 0 => (w.after.len(), 21),
                LastRowThen::Rests | LastRowThen::Gapped => (w.boxed, 252),
                LastRowThen::Erased { .. } => (w.then_on, 22),
                LastRowThen::Back => (w.then_on, 190),
                LastRowThen::Paused => (w.then_on, 252),
                LastRowThen::Opened => unreachable!("no control is taken where a seat opens"),
            };
            assert!(
                met >= bites,
                "negative control, {why}: with no seat it met the letters in only {met} frames \
                 ({w:?})"
            );
            continue;
        }
        if i32::from(room) >= seated {
            assert!(
                w.drawn == w.held && w.deep + w.inked + w.careted == 0,
                "{why}: seated, it left the glass or lay on the letters or the caret ({w:?})"
            );
            continue;
        }
        let first = w
            .first
            .unwrap_or_else(|| panic!("fixture: {why}: the line never met the box ({w:?})"));
        let gone = w
            .gone
            .unwrap_or_else(|| panic!("{why}: the companion never left the glass ({w:?})"));
        // The wall's cut (`FADE_OFF_THE_LETTERS`, 64 ms: 4 frames, the 4th off the glass) and a
        // frame.
        assert!(
            w.rose.is_empty() && gone <= first + 64 + 16,
            "{why}: from the box's meeting the letters at {first} ms its fade rose at {:?}, or \
             took to {gone} ms",
            w.rose
        );
        assert!(
            w.faded_on <= 3,
            "{why}: its fade lay on the letters or the caret in {} frames ({w:?})",
            w.faded_on
        );
        assert!(
            w.full <= 1,
            "{why}: past the line, it lay on the letters at full alpha in {} frames ({w:?})",
            w.full
        );
        assert!(
            w.relaid.is_empty(),
            "{why}: on glass after its fade, on the letters or the caret: {:?}",
            w.relaid
        );
        match then {
            LastRowThen::Erased { keys, after } if i32::from(erased_room(room, keys)) >= seated => {
                let erased = w.last_key + after + 50 * u64::from(keys - 1);
                assert!(
                    w.returned.is_some_and(|r| r <= erased.max(gone) + 1_000),
                    "{why}: the erase left it its room and it was not back on glass a second \
                     past it ({w:?})"
                );
            }
            LastRowThen::Opened => {
                let opened = w.last_key + 1_500;
                assert!(
                    w.returned.is_some_and(|r| r <= opened.max(gone) + 1_000),
                    "{why}: a seat opened beside the line and it was not back on glass a second \
                     past it ({w:?})"
                );
            }
            _ => assert!(
                w.unfaded.is_empty(),
                "{why}: on glass after its fade, the line under it: {:?}",
                w.unfaded
            ),
        }
        assert!(
            w.back.is_some_and(|b| b <= w.until.max(gone) + 1_000),
            "{why}: not back on glass a second past the wrap or the Enter ({w:?})"
        );
        // Off the line on the row the seat opened over it, the companion has the Enter's scroll
        // carry the line up under it for the frame before it moves (the same in the effects
        // replay at four blank cells, which seat it, and with the law reverted): that frame is
        // the scroll's, not the wall's.
        let scrolled_under = |ms: u64| then == LastRowThen::Opened && ms <= w.until + 16;
        assert!(
            w.after.iter().all(|&(ms, _)| scrolled_under(ms)),
            "{why}: past the wrap or the Enter, frames on the letters or the caret: {:?}",
            w.after
        );
    }
}

/// THE MARGIN KEEPS THE COMPANION OFF THE LINE (the trail gallery's `fold-rest` and `focus-wrap`):
/// writing that runs into the margin, stops short of it and wraps, the body at the wall under it
/// from before the letters come. Past the first keys (typed under a body that is getting going,
/// r24 K7-1), no frame's drawn box meets a lettered cell on any row or the caret's cell. Every
/// companion style on every page. Negative controls: [`OldPath::MarginKeepsTheLine`] reverted
/// alone, the letters run under the body at the wall and its crossing flies out through them;
/// [`OldPath::CrossingRisesIntoTheLine`] reverted alone, on [`Scene::StatusWrap`], whose crossing
/// lands a row lower, the body rises from under the line into it as it leaves.
#[test]
fn the_margin_keeps_the_companion_off_the_line() {
    // The run's start: the first keys typed under a body getting going (r24 K7-1) and its catch-up
    // to the writing's 20 cells a second, in which its stride swung its tail into the caret's cell
    // 360 ms after the first key on `FocusWrap` (10 px, a frame).
    const FIRST_KEYS: u64 = 400;
    let hits = |style: &str, look: Look, scene: Scene, old: &[OldPath]| -> Vec<String> {
        coats_driven(style, look, scene, old, Drive::Ctl)
            .iter()
            .filter(|j| {
                j.ms >= TYPING_FROM + FIRST_KEYS
                    && (!j.coat.box_cells.is_empty() || j.coat.ink_px > 0 || j.coat.caret_px > 0)
            })
            .map(|j| format!("{} ms, box {:?}: {:?}", j.ms, j.coat.pet, j.coat.box_cells))
            .collect()
    };
    for scene in [Scene::FoldRest, Scene::FocusWrap, Scene::StatusWrap] {
        for look in [DARK, SOLARIZED_DARK, GITHUB_LIGHT, DEFAULT_LIGHT] {
            for style in COMPANIONS {
                let met = hits(style, look, scene, &[]);
                assert!(
                    met.is_empty(),
                    "{style} on {}, {scene:?}: {} frames on the writing: {met:?}",
                    look.name,
                    met.len()
                );
            }
        }
        let old = hits("rainbow + cat", DARK, scene, &[OldPath::MarginKeepsTheLine]);
        assert!(
            old.len() > 10,
            "negative control: {scene:?} without the margin's slide met the writing in {} frames",
            old.len()
        );
    }
    let old = hits(
        "rainbow + cat",
        DARK,
        Scene::StatusWrap,
        &[OldPath::CrossingRisesIntoTheLine],
    );
    assert!(
        old.len() > 5,
        "negative control: a crossing drawn whole off the row under the line met it in {} frames",
        old.len()
    );
}

/// A scene's events and its tail, ms ([`Scene::events`]).
pub type Played = (Vec<(u64, Ev)>, u64);

/// A frame [`off_the_caret_and_letters`] finds: its ms, the companion's box, its px on the caret's
/// cell and the lettered cells its box meets.
type Met = (
    u64,
    Option<(i32, i32, i32, i32)>,
    usize,
    Vec<(usize, usize)>,
);

/// The trail gallery's `last-row` page and keys on a grid `cols` wide ([`Scene::LastRow`] at the
/// gallery's 72): its full rows cut to the width, the burst typed along the last row up to two
/// cells short of a 72-column wall, one short at 71 and none at 70, the hand stopped, then the
/// Enter whose answer scrolls the page. The burst's last key and the Enter, ms.
pub fn last_row_at(cols: u16) -> (Played, (u64, u64)) {
    use crate::trail_letters_read::{BURST_TEXT, LAST_ROW_FULL, LAST_ROW_RAGGED, ROWS};
    let mut page = String::from("\x1b[H\x1b[2J");
    for over in (1..usize::from(ROWS)).rev() {
        let text: String = if over % 2 == 1 {
            LAST_ROW_FULL
                .chars()
                .cycle()
                .take(usize::from(cols))
                .collect()
        } else {
            LAST_ROW_RAGGED[over / 2 % LAST_ROW_RAGGED.len()]
                .chars()
                .take(usize::from(cols))
                .collect()
        };
        page.push_str(&text);
        page.push_str("\r\n");
    }
    page.push_str("$ ");
    let mut ev = vec![(200, Ev::Out(page.into_bytes()))];
    ev.extend(typed(BURST_TEXT, 1_000, 50));
    let last = 1_000 + 50 * (BURST_TEXT.len() as u64 - 1);
    let enter = last + 50 + 6_000;
    ev.push((enter, Ev::Key('\r', b"\r\n".to_vec())));
    ev.push((enter + 6, Ev::Out(b"Below: pqgyj [ok]\r\n$ ".to_vec())));
    ev.extend(typed(TYPING_TEXT, enter + 470, 60));
    ((ev, 6_000), (last, enter))
}

/// The trail gallery's `flood` (`aterm-lab`'s `flood_frames`) on a grid `cols` wide, typed to
/// its last cell ([`flood_keys`]): no seat on the page. The last key, ms.
pub fn flood_at(cols: u16) -> (Played, u64) {
    let cells = usize::from(crate::trail_letters_read::ROWS) * usize::from(cols);
    flood_keys(cols, cells)
}

/// The keys the trail gallery's `flood` types whatever the grid's width (`aterm-lab`'s
/// `FLOOD_KEYS`): its default grid, 72 columns, once.
const FLOOD_KEYS: usize = 72 * 14;

/// The trail gallery's `flood` (`aterm-lab`'s `flood_frames` and `FloodScreen`) on a grid `cols`
/// wide, `keys` keys long: a screen of words, each in its own ink, a space between them and no
/// blank run a body fits in, written over from the top a cell a key, up to 24 keys a 50 ms frame
/// to the row's last cell but one, then the three that take the caret over the wrap, and from
/// the top again past the screen's last cell. The last key, ms.
fn flood_keys(cols: u16, keys: usize) -> (Played, u64) {
    const WORDS: [&str; 12] = [
        "gypsy", "pygmy", "jiggy", "foggy", "poppy", "guppy", "goopy", "pudgy", "soggy", "yuppy",
        "gappy", "jumpy",
    ];
    let ink = |n: usize| -> [u8; 3] {
        let hue = (n % 360 * 137) % 360;
        let up = (40 + 150 * (hue % 60) / 60) as u8;
        let down = (190 - 150 * (hue % 60) / 60) as u8;
        match hue / 60 {
            0 => [190, up, 40],
            1 => [down, 190, 40],
            2 => [40, 190, up],
            3 => [40, down, 190],
            4 => [up, 40, 190],
            _ => [190, 40, down],
        }
    };
    let rows = crate::trail_letters_read::ROWS;
    let cells = usize::from(rows) * usize::from(cols);
    let mut page = String::from("\x1b[H\x1b[2J");
    for place in 0..cells / 6 {
        let [r, g, b] = ink(place);
        page.push_str(&format!(
            "\x1b[38;2;{r};{g};{b}m{}\x1b[0m ",
            WORDS[place % 12]
        ));
    }
    page.push_str("\x1b[H");
    let mut ev = vec![(200, Ev::Out(page.into_bytes()))];
    let text: Vec<char> = (0..keys)
        .map(|key| {
            let word = WORDS[(key / 6 + 1) % 12];
            word.as_bytes().get(key % 6).map_or(' ', |&l| char::from(l))
        })
        .collect();
    let (mut t, mut col, mut frame, mut last) = (0usize, 0usize, 0u64, 0u64);
    let c = usize::from(cols);
    while t < keys {
        let run = c.saturating_sub(2).saturating_sub(col);
        let (n, to) = if run == 0 {
            (3, 1)
        } else {
            (run.min(24), col + run.min(24))
        };
        for k in 0..n.min(keys - t) {
            let cell = t % cells;
            let [r, g, b] = ink(cells / 6 + cell / 6);
            let mut bytes = if cell == 0 && t > 0 {
                b"\x1b[H".to_vec()
            } else {
                Vec::new()
            };
            bytes.extend_from_slice(format!("\x1b[38;2;{r};{g};{b}m").as_bytes());
            bytes.push(text[t] as u8);
            bytes.extend_from_slice(b"\x1b[0m");
            last = 1_000 + frame * 50 + k as u64 / 8;
            ev.push((last, Ev::Key(text[t], bytes)));
            t += 1;
        }
        col = to;
        frame += 1;
    }
    ((ev, 3_500), last)
}

/// `events` replayed for `style` on `look` on the gallery's rows and `cols` columns, a frame
/// every 16 ms, keys as `drive` types them ([`Drive::Console`] or [`Drive::Ctl`]), its laws in
/// `old` reverted.
pub fn walled(
    (style, look): (&str, Look),
    cols: u16,
    drive: Drive,
    (ev, tail): &Played,
    old: &[OldPath],
) -> Vec<Judged> {
    let rows = crate::trail_letters_read::ROWS;
    let mut r = Replay::gridded(style, look, TrailColor::Automatic, rows, cols);
    r.coat = true;
    r.keys_unnoted = drive != Drive::Console;
    r.fx.set_sparkle_enabled(true);
    r.old_paths = old.to_vec();
    r.run(ev, *tail)
}

/// Whether a frame's companion lies on the caret's cell: its art drawn under the letters there
/// ([`Coat::caret_px`]), or its box over the cell as the brain judges one ([`Coat::box_on_caret`]),
/// which reads a body drawn over blank ground too, where the knock layer has no runs.
fn on_the_caret(c: &Coat) -> bool {
    c.pet.is_some() && (c.caret_px > 0 || c.box_on_caret)
}

/// Frames in `judged` from `from` to `to` ms whose companion lies on the caret's cell
/// ([`on_the_caret`]), whose box meets a lettered cell, or that draw no companion: `(ms, box,
/// caret px, cells)`.
fn off_the_caret_and_letters(judged: &[Judged], (from, to): (u64, u64)) -> Vec<Met> {
    judged
        .iter()
        .filter(|j| (from..to).contains(&j.ms))
        .filter(|j| on_the_caret(&j.coat) || !j.coat.box_cells.is_empty() || j.coat.pet.is_none())
        .map(|j| (j.ms, j.coat.pet, j.coat.caret_px, j.coat.box_cells.clone()))
        .collect()
}

/// Frames in `judged` from `from` to `to` ms that draw the companion on glass on the caret's cell
/// or with its box on a lettered cell: [`off_the_caret_and_letters`] less the frames off the glass.
fn on_the_caret_or_letters(judged: &[Judged], span: (u64, u64)) -> Vec<Met> {
    off_the_caret_and_letters(judged, span)
        .into_iter()
        .filter(|m| m.1.is_some())
        .collect()
}

/// THE CARET'S CELL IS NO SEAT AT THE WALL (W3, the trail gallery's `last-row` at 70 and 71
/// columns): a burst typed along a full page's last row up to the wall, where the body stands
/// fitted under the full row over it, and the hand stopped. At 72 columns two cells lie past the
/// caret and the fitted body at the wall keeps off its cell; at 71 one, at 70 none, and the body
/// there lay in the caret's cell. Two laws keep it off, in turn:
/// - The wall that holds no seat hides it ([`OldPath::NoSeatHidesTheBody`],
///   [`a_last_row_that_holds_no_seat_hides_the_companion`]) from the key whose letter meets the
///   box it draws at rest there, while the hand writes, and holds it off the glass to the Enter.
/// - Under that, the caret's cell is no seat ([`OldPath::CaretCellIsNoSeat`]): where the box the
///   body draws at the wall lies on the caret's cell, the margin sends it to the nearest seat off
///   the line once the hand's rhythm closes (a pause the next key may end with a wrap, whose
///   crossing is the body's one flight, keeps the wall), fitted between two full rows; on a page
///   with no seat anywhere the wall side turns (`a_watched_page_s_caret_takes_its_cell_back`).
///
/// With every law, from the time the body takes to leave the wall after the rhythm closes on the
/// burst's last key (900 ms: the rhythm's 600 and the flight) to the Enter, at every width, no
/// frame draws the companion on the caret's cell or its box on a lettered cell: at 71 and 70 the
/// hide has it off the glass. With the hide reverted alone, the same frames draw it in every one,
/// on the seat off the line, and none of them on the caret's cell or a lettered cell.
///
/// AND THE SEAT RIDES THE PAGE ([`OldPath::SeatRidesThePage`]): the Enter and its answer scroll the
/// page two rows under that seat. From the frame after the second row came up (40 ms on: the host
/// reads the page a frame behind its glass) until the body sets off for the caret's new line (400
/// ms on), no frame lays it on the caret's cell or its box on a lettered cell either: it rode up
/// with the rows it sat among. Judged with every law, which at 71 and 70 columns has the body off
/// the glass at the Enter, so there the ride is judged with the hide reverted, where the body sits
/// on that seat; [`a_wall_over_blank_ground_is_no_seat_and_its_seat_rides_the_page`] judges it
/// with every law.
///
/// Every companion style on every page, the keys typed through the console and, as the gallery
/// types them, past it. Negative controls, each with the hide reverted (with it, at 71 and 70
/// columns, the body is off the glass whatever either law does):
/// [`OldPath::CaretCellIsNoSeat`] reverted too, at 71 and 70 columns it rests in the caret's cell;
/// [`OldPath::SeatRidesThePage`] reverted too, it stays on its row of the glass, on the letters
/// of the line it left.
#[test]
fn at_the_wall_the_caret_s_cell_is_no_seat() {
    const LEAVES: u64 = 900;
    const RIDDEN: u64 = 40;
    const SETS_OFF: u64 = 400;
    const HIDE: OldPath = OldPath::NoSeatHidesTheBody;
    for drive in [Drive::Console, Drive::Ctl] {
        for cols in [72, 71, 70] {
            let (scene, (last, enter)) = last_row_at(cols);
            let walls = (last + LEAVES, enter);
            let ridden = (enter + RIDDEN, enter + SETS_OFF);
            for look in [DARK, SOLARIZED_DARK, GITHUB_LIGHT, DEFAULT_LIGHT] {
                for style in COMPANIONS {
                    let judged = walled((style, look), cols, drive, &scene, &[]);
                    for (span, when) in [(walls, ""), (ridden, " after the Enter")] {
                        let met = on_the_caret_or_letters(&judged, span);
                        assert!(
                            met.is_empty(),
                            "{style} on {} at {cols} columns ({drive:?}): {} frames{when} on the \
                             caret or the letters: {:?}",
                            look.name,
                            met.len(),
                            &met[..met.len().min(4)]
                        );
                    }
                    let judged = walled((style, look), cols, drive, &scene, &[HIDE]);
                    for (span, when) in [(walls, ""), (ridden, " after the Enter")] {
                        let met = off_the_caret_and_letters(&judged, span);
                        assert!(
                            met.is_empty(),
                            "{style} on {} at {cols} columns ({drive:?}), the hide reverted: {} \
                             frames{when} on the caret, the letters or off the glass: {:?}",
                            look.name,
                            met.len(),
                            &met[..met.len().min(4)]
                        );
                    }
                }
            }
            if cols < 72 {
                let old = walled(
                    ("rainbow + cat", DARK),
                    cols,
                    drive,
                    &scene,
                    &[HIDE, OldPath::CaretCellIsNoSeat],
                );
                let on_caret = old
                    .iter()
                    .filter(|j| (last + LEAVES..enter).contains(&j.ms) && on_the_caret(&j.coat))
                    .count();
                assert!(
                    on_caret > 100,
                    "negative control ({drive:?}): at {cols} columns the wall kept the body off \
                     the caret's cell ({on_caret} frames on it)"
                );
                let stayed = off_the_caret_and_letters(
                    &walled(
                        ("rainbow + cat", DARK),
                        cols,
                        drive,
                        &scene,
                        &[HIDE, OldPath::SeatRidesThePage],
                    ),
                    ridden,
                )
                .len();
                assert!(
                    stayed > 10,
                    "negative control ({drive:?}): at {cols} columns the body kept off the \
                     letters after the Enter unridden ({stayed} frames on them)"
                );
            }
        }
    }
}

/// A WATCHED PAGE'S CARET TAKES ITS CELL BACK (W4, the trail gallery's `flood` at 72 and 70
/// columns): a screen of words typed over to its last cell, no seat on it, the caret coming into
/// the body at the wall.
/// - Typed through the console, as the gallery's instance has them, the margin holds the body to
///   the wall while the hand writes, and the box it draws at rest there meets the line's last
///   letters: the wall's hide takes it off the glass until the line ends
///   ([`OldPath::NoSeatAnywhereHides`]), the station on the caret's line side lying on those
///   letters too. From [`REST`] after the last key to the end no frame draws its box on a
///   lettered cell.
/// - Typed past the console, the watch holds it where it sits until the caret comes into it, then
///   lets it go to the station on the caret's line side, over the letters it has run on all
///   along, drawn under them; it never leaves the glass: whole at its fit, beside the caret, and
///   not a sliver behind it (the "crumb").
///
/// Either way, from the time it takes to step off (300 ms after the last key) to the end, no
/// frame lays it on the caret's cell ([`on_the_caret`]). Every companion style, on every page
/// past the console and on dark through it. Negative controls: [`OldPath::CaretCellIsNoSeat`]
/// reverted alone, the watch or the margin holds it in the caret's cell;
/// [`OldPath::NoSeatAnywhereHides`] reverted alone, through the console the wall side turns and
/// the body rests on the line's letters at the station.
#[test]
fn a_watched_page_s_caret_takes_its_cell_back() {
    const STEPS: u64 = 300;
    for drive in [Drive::Console, Drive::Ctl] {
        for cols in [72, 70] {
            let (scene, last) = flood_at(cols);
            let end = last + scene.1;
            // Typed through the console, each key reads the whole page: on dark alone, where
            // the body is placed as on every page.
            let looks: &[Look] = if drive == Drive::Console {
                &[DARK]
            } else {
                &[DARK, SOLARIZED_DARK, GITHUB_LIGHT, DEFAULT_LIGHT]
            };
            for &look in looks {
                for style in COMPANIONS {
                    let judged = walled((style, look), cols, drive, &scene, &[]);
                    let met: Vec<_> = judged
                        .iter()
                        .filter(|j| (last + STEPS..end).contains(&j.ms))
                        .filter(|j| {
                            on_the_caret(&j.coat)
                                || if drive == Drive::Console {
                                    j.ms >= last + REST && !j.coat.box_cells.is_empty()
                                } else {
                                    j.coat.pet.is_none()
                                }
                        })
                        .map(|j| (j.ms, j.coat.pet, j.coat.caret_px, j.coat.box_cells.len()))
                        .collect();
                    assert!(
                        met.is_empty(),
                        "{style} on {} at {cols} columns ({drive:?}): {} frames on the caret's \
                         cell, {}: {:?}",
                        look.name,
                        met.len(),
                        if drive == Drive::Console {
                            "or at rest on the letters"
                        } else {
                            "or off the glass"
                        },
                        &met[..met.len().min(4)]
                    );
                }
            }
            let old = walled(
                ("rainbow + cat", DARK),
                cols,
                drive,
                &scene,
                &[OldPath::CaretCellIsNoSeat],
            );
            let on_caret = old
                .iter()
                .filter(|j| (last + STEPS..end).contains(&j.ms) && on_the_caret(&j.coat))
                .count();
            assert!(
                on_caret > 10,
                "negative control ({drive:?}): at {cols} columns the body was let off the \
                 caret's cell ({on_caret} frames on it)"
            );
            if drive == Drive::Console {
                let old = walled(
                    ("rainbow + cat", DARK),
                    cols,
                    drive,
                    &scene,
                    &[OldPath::NoSeatAnywhereHides],
                );
                let on_letters = old
                    .iter()
                    .filter(|j| {
                        (last + REST..end).contains(&j.ms)
                            && j.coat.pet.is_some()
                            && !j.coat.box_cells.is_empty()
                    })
                    .count();
                assert!(
                    on_letters > 100,
                    "negative control: at {cols} columns the body rested off the letters with \
                     the wall side turned ({on_letters} frames on them)"
                );
            }
        }
    }
}

/// THE WALL SIDE TURNS IN THE ROW'S LAST CELL (W4, the trail gallery's `flood` at 70 columns as
/// the gallery types it, [`FLOOD_KEYS`] keys: past the screen's last cell its last keys fold the
/// caret home to the top row). At each row's end the caret stands a frame in the cell before the
/// last, inside the box the body draws at the wall, before the next keys take it over the wrap;
/// the margin holds the wall side there, so the body stays at the wall, where its box meets
/// fewer of the last row's letters than at the station on the caret's line side. From the frame
/// the caret came into the last row's cell before the last (three frames before the last key)
/// to 470 ms after the last key, while the stream's watch holds it, no frame draws its box a
/// cell or more short of the wall. Every companion style, on dark, the keys typed through the
/// console, as the gallery's instance has them. Negative control: [`OldPath::TurnAtTheLastCell`]
/// reverted alone, the wall side turns in the cell before the last and the body walks along the
/// last row's letters to the station as the caret folds home.
#[test]
fn the_wall_side_turns_in_the_row_s_last_cell() {
    const COLS: u16 = 70;
    const HELD: u64 = 470;
    let (scene, last) = flood_keys(COLS, FLOOD_KEYS);
    let span = (last - 150, last + HELD);
    let (cw, _) = gallery_cell();
    let wall = i32::from(COLS) * cw as i32;
    let short = |judged: &[Judged]| {
        judged
            .iter()
            .filter(|j| (span.0..=span.1).contains(&j.ms))
            .filter(|j| j.coat.pet.is_none_or(|(_, x1, ..)| x1 + cw as i32 <= wall))
            .map(|j| (j.ms, j.coat.pet, j.coat.box_cells.len()))
            .collect::<Vec<_>>()
    };
    for style in COMPANIONS {
        let met = short(&walled((style, DARK), COLS, Drive::Console, &scene, &[]));
        assert!(
            met.is_empty(),
            "{style} at {COLS} columns: {} frames off the glass or a cell short of the wall: {:?}",
            met.len(),
            &met[..met.len().min(4)]
        );
    }
    let old = short(&walled(
        ("rainbow + cat", DARK),
        COLS,
        Drive::Console,
        &scene,
        &[OldPath::TurnAtTheLastCell],
    ));
    assert!(
        old.len() > 10,
        "negative control: at {COLS} columns the body kept the wall with the wall side turned in \
         the cell before the last ({} frames short of it): {:?}",
        old.len(),
        &old[..old.len().min(4)]
    );
}

/// How long after a flood's last key the body typed through the console is off the letters
/// ([`a_watched_page_s_caret_takes_its_cell_back`]): the stream's watch holds it until the writing
/// has lapsed 800 ms, the wall's hide then takes it and the letters cut its fade within 64 ms
/// (`FADE_OFF_THE_LETTERS`). The trail gallery's `l1650` and `l3000` lie past it.
const REST: u64 = 900;

/// The trail gallery's `last-row` page on a grid `cols` wide ([`last_row_at`]) with the burst's
/// last word typed as spaces, so the line's last letters stop six cells short of the caret and the
/// box the body draws at the wall meets no letter, and the Enter answered by the prompt alone, so
/// the page scrolls one row. The last key and the Enter, ms.
fn last_row_spaced(cols: u16) -> (Played, (u64, u64)) {
    use crate::trail_letters_read::BURST_TEXT;
    const SPACES: usize = 6;
    let ((mut ev, tail), (last, enter)) = last_row_at(cols);
    let text: String = BURST_TEXT
        .chars()
        .take(BURST_TEXT.len() - SPACES)
        .chain(std::iter::repeat_n(' ', SPACES))
        .collect();
    ev.retain(|(at, e)| *at < 1_000 || *at > last || !matches!(e, Ev::Key(..)));
    ev.extend(typed(&text, 1_000, 50));
    for (at, e) in &mut ev {
        if *at == enter + 6 {
            *e = Ev::Out(b"$ ".to_vec());
        }
    }
    ev.sort_by_key(|(at, _)| *at);
    ((ev, tail), (last, enter))
}

/// A WALL OVER BLANK GROUND IS NO SEAT EITHER, AND ITS SEAT RIDES THE PAGE (W3's laws where the
/// wall's hide does not act): the gallery's `last-row` page with the line's last six keys spaces
/// ([`last_row_spaced`]). At 71 and 70 columns the box the body draws at the wall lies on the
/// caret's cell and meets no letter, so the wall holds a seat to the hide and the caret's cell law
/// is the one that acts ([`OldPath::CaretCellIsNoSeat`]): once the hand's rhythm closes the body
/// flies to the nearest seat off the line, the row two up. From the time that takes (900 ms after
/// the last key) to the Enter, no frame draws it off the glass, on the caret's cell by its box
/// ([`on_the_caret`]: drawn over blank ground, the knock layer reads it nowhere) or on a lettered
/// cell, and at the Enter it sits over the caret's row. At 72 columns the box at the wall keeps
/// off the caret's cell and the body waits there, judged the same.
///
/// The Enter is answered by the prompt alone, and the page scrolls one row under the seat, which
/// comes to hold the full row's letters: from the frame after the row came up (40 ms on) until
/// the body sets off for the caret's new line (400 ms on), no frame lays it on the caret's cell
/// or its box on a lettered cell: it rode up with the rows it sat among
/// ([`OldPath::SeatRidesThePage`]).
///
/// Every law, every companion style on every page, the keys typed through the console and past
/// it. Negative controls, each reverting one law alone: [`OldPath::CaretCellIsNoSeat`], at 71 and
/// 70 columns the body rests at the wall with its box on the caret's cell; and
/// [`OldPath::SeatRidesThePage`], it stays on its row of the glass after the Enter, on the letters
/// the scroll brought there.
#[test]
fn a_wall_over_blank_ground_is_no_seat_and_its_seat_rides_the_page() {
    const LEAVES: u64 = 900;
    const RIDDEN: u64 = 40;
    const SETS_OFF: u64 = 400;
    for drive in [Drive::Console, Drive::Ctl] {
        for cols in [72, 71, 70] {
            let (scene, (last, enter)) = last_row_spaced(cols);
            let walls = (last + LEAVES, enter);
            let ridden = (enter + RIDDEN, enter + SETS_OFF);
            let (_, ch) = gallery_cell();
            let caret_top = i32::from(crate::trail_letters_read::ROWS - 1) * ch as i32;
            for look in [DARK, SOLARIZED_DARK, GITHUB_LIGHT, DEFAULT_LIGHT] {
                for style in COMPANIONS {
                    let judged = walled((style, look), cols, drive, &scene, &[]);
                    for (span, when) in [(walls, ""), (ridden, " after the Enter")] {
                        let met = off_the_caret_and_letters(&judged, span);
                        assert!(
                            met.is_empty(),
                            "{style} on {} at {cols} columns ({drive:?}): {} frames{when} on the \
                             caret, the letters or off the glass: {:?}",
                            look.name,
                            met.len(),
                            &met[..met.len().min(4)]
                        );
                    }
                    if cols < 72 {
                        let seat = judged
                            .iter()
                            .rfind(|j| j.ms < enter)
                            .and_then(|j| j.coat.pet);
                        assert!(
                            seat.is_some_and(|(.., y1)| y1 <= caret_top),
                            "{style} on {} at {cols} columns ({drive:?}): at the Enter the body \
                             is not seated over the caret's row (top {caret_top}): {seat:?}",
                            look.name
                        );
                    }
                }
            }
            if cols < 72 {
                let old = walled(
                    ("rainbow + cat", DARK),
                    cols,
                    drive,
                    &scene,
                    &[OldPath::CaretCellIsNoSeat],
                );
                let on_caret = old
                    .iter()
                    .filter(|j| (last + LEAVES..enter).contains(&j.ms) && on_the_caret(&j.coat))
                    .count();
                assert!(
                    on_caret > 100,
                    "negative control ({drive:?}): at {cols} columns the wall kept the body off \
                     the caret's cell ({on_caret} frames on it)"
                );
                let stayed = on_the_caret_or_letters(
                    &walled(
                        ("rainbow + cat", DARK),
                        cols,
                        drive,
                        &scene,
                        &[OldPath::SeatRidesThePage],
                    ),
                    ridden,
                )
                .len();
                assert!(
                    stayed > 10,
                    "negative control ({drive:?}): at {cols} columns the body kept off the \
                     letters after the Enter unridden ({stayed} frames on them)"
                );
            }
        }
    }
}
