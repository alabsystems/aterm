// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The trail gallery's `steam` scene on the alternate screen, replayed through the effects
//! pipeline on fixed cells, so each law it pins is judged on every face's geometry wherever
//! the tests run (the render tests replay it on the one face their platform bundles).

use aterm_core::render::RenderInput;
use aterm_core::terminal::{ClockReading, Rgb, Terminal};

use super::{OldPath, PetInputKind};
use crate::cursor_fx::TrailColor;
use crate::cursor_glow::GlowStyle;
use crate::pipeline::EffectsPipeline;

const ROWS: u16 = 14;
const COLS: u16 = 72;
const FRAME_MS: u64 = 16;
const TYPING_TEXT: &str = "echo the quick brown fox";
const TYPING_FROM: u64 = 1_000;
/// From when (ms) frames are judged: the companion has come to the prompt.
const FROM: u64 = 600;
/// The cells (px) the scenes replay on: Linux's bundled JetBrains Mono (13 x 29), the trail
/// gallery's 14 x 26, and Iosevka Term (11 x 28), the face every platform bundles.
const CELLS: [(usize, usize); 3] = [(13, 29), (14, 26), (11, 28)];
/// The companions judged: the cat, and the dog.
const STYLES: [&str; 2] = ["rainbow kitty pet", "rainbow dog"];
/// The prompt's row (0-based): the page's rows above and under it are lettered.
const PROMPT_ROW: usize = 4;

/// The `steam` scene's rows over and under the prompt: the gallery's own, and a row of
/// descenders over a sentence.
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

#[derive(Clone, Debug)]
enum Ev {
    Out(Vec<u8>),
    Key(char, Vec<u8>),
}

/// A drawn frame: when, the companion's box (`x0, x1, y0, y1` px), and the lettered cells
/// (`row, col`) the box meets by a pixel or more.
#[derive(Clone, Debug)]
struct Frame {
    ms: u64,
    pet: Option<(i32, i32, i32, i32)>,
    boxed: Vec<(usize, usize)>,
}

/// `events` replayed on `cell` px cells, a frame every [`FRAME_MS`] to `tail` ms past the last,
/// the laws in `old` reverted. A key's echo lands 2 ms after it; typed at the window
/// (`ctl` false) the console sees each key, as the gallery types it (`aterm ctl key`) none.
fn replay(
    cell: (usize, usize),
    style: &str,
    events: &[(u64, Ev)],
    tail: u64,
    old: &[OldPath],
    ctl: bool,
) -> Vec<Frame> {
    let (cw, ch) = cell;
    let mut term = Terminal::new(ROWS, COLS);
    let rgb = |c: u32| Rgb {
        r: (c >> 16) as u8,
        g: (c >> 8) as u8,
        b: c as u8,
    };
    term.set_default_foreground(rgb(0x00D0_D0D0));
    term.set_default_background(rgb(0x0011_1318));
    term.set_default_cursor_color(Some(rgb(0x0050_FA7B)));
    let mut fx = EffectsPipeline::new();
    fx.set_focused(true);
    fx.set_cursor_glow(
        true,
        style,
        TrailColor::Automatic,
        None,
        260,
        24,
        1.0,
        0.6,
        true,
        0x0050_FA7B,
    );
    fx.set_cursor_pet(GlowStyle::style_names_any_pet(style), 0x5EED);
    fx.set_sparkle_enabled(true);
    for &law in old {
        fx.set_pet_old_path(law, true);
    }
    let mut timeline: Vec<(u64, u8, usize)> = Vec::new();
    for (i, (at, ev)) in events.iter().enumerate() {
        timeline.push((*at, 0, i));
        if matches!(ev, Ev::Key(..)) {
            timeline.push((*at + 2, 1, i));
        }
    }
    let end = events.last().map_or(0, |e| e.0) + tail;
    let mut f = FRAME_MS;
    while f <= end {
        timeline.push((f, 2, 0));
        f += FRAME_MS;
    }
    timeline.sort_unstable();
    let (rows, cols) = (usize::from(ROWS), usize::from(COLS));
    let mut clock = 0u64;
    let mut frames = Vec::new();
    for (at, kind, i) in timeline {
        if at > clock {
            fx.advance((at - clock) as f64);
            clock = at;
        }
        match (kind, &events[i].1) {
            (0, Ev::Key(..)) if ctl => {}
            (0, Ev::Key('\r', _)) => {
                fx.note_console_input(PetInputKind::Submit);
                fx.note_keystroke();
            }
            (0, Ev::Key('\u{7f}', _)) => {
                fx.note_console_input(PetInputKind::Delete);
                fx.note_keystroke();
            }
            (0, Ev::Key(c, _)) => {
                let input = term.cell_frame(rows, cols);
                let _ = fx.note_committed_char(&mut term, &input, *c);
            }
            (0, Ev::Out(bytes)) | (1, Ev::Key(_, bytes)) => {
                let at = ClockReading {
                    monotonic: fx.now(),
                    wall_ms: None,
                };
                term.process_at(bytes, at);
            }
            (2, _) => {
                let mut input: RenderInput = term.cell_frame(rows, cols);
                fx.apply(&mut term, &mut input, cw, ch);
                let pet = fx.companion_hit_rect();
                let mut boxed = Vec::new();
                if let Some((x0, x1, y0, y1)) = pet {
                    for (r, line) in input.cells.iter().enumerate() {
                        for (c, cl) in line.iter().enumerate().take(cols) {
                            if cl.ch == ' ' || cl.ch == '\0' || cl.ch.is_control() {
                                continue;
                            }
                            let (cx, cy) = ((c * cw) as i32, (r * ch) as i32);
                            if cx < x1 && cx + cw as i32 > x0 && cy < y1 && cy + ch as i32 > y0 {
                                boxed.push((r, c));
                            }
                        }
                    }
                }
                frames.push(Frame {
                    ms: clock,
                    pet,
                    boxed,
                });
            }
            _ => {}
        }
    }
    frames
}

/// `text` typed from `start` ms, a key every `every` ms.
fn typed(text: &str, start: u64, every: u64) -> Vec<(u64, Ev)> {
    text.chars()
        .enumerate()
        .map(|(i, c)| {
            let mut b = [0u8; 4];
            (
                start + i as u64 * every,
                Ev::Key(c, c.encode_utf8(&mut b).as_bytes().to_vec()),
            )
        })
        .collect()
}

/// The `steam` page on the alternate screen: `above` over the prompt's row, `below` under it,
/// the caret on the prompt's row after `$ `.
fn steam_page((above, below): (&str, &str)) -> String {
    format!("\x1b[?1049h\x1b[2J\x1b[4H{above}\x1b[6H{below}\x1b[5H$ ")
}

/// The scene: the page, then [`TYPING_TEXT`] typed from [`TYPING_FROM`] a key every 60 ms,
/// twelve Backspaces 300 ms after it, 50 ms apart, and each of `then` (ms after the last
/// Backspace, the program's bytes); 1.6 s past the last.
fn steamed(
    cell: (usize, usize),
    style: &str,
    rows: (&str, &str),
    old: &[OldPath],
    ctl: bool,
    then: &[(u64, &[u8])],
) -> Vec<Frame> {
    let mut ev = vec![(0, Ev::Out(steam_page(rows).into_bytes()))];
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
    replay(cell, style, &ev, 1_600, old, ctl)
}

/// The frames (ms) from [`FROM`] whose box meets a lettered cell of `row`.
fn on_letters_of(frames: &[Frame], row: usize) -> Vec<u64> {
    frames
        .iter()
        .filter(|f| f.ms >= FROM && f.boxed.iter().any(|c| c.0 == row))
        .map(|f| f.ms)
        .collect()
}

/// How many frames from [`FROM`] have the box's foot past the row under the prompt (cells `ch`
/// px tall).
fn past_the_row_under(frames: &[Frame], ch: usize) -> usize {
    let under_end = ((PROMPT_ROW + 2) * ch) as i32;
    frames
        .iter()
        .filter(|f| f.ms >= FROM && f.pet.is_some_and(|p| p.3 > under_end))
        .count()
}

/// A FULL-SCREEN PROGRAM'S ROWS UNDER ITS LINE ARE ITS OWN ([`OldPath::FullScreenKeepsItsRow`]),
/// on every face's cells ([`CELLS`]): in the `steam` scene on the alternate screen the cat
/// keeps its line's row. From 600 ms, the cat and the dog, between the gallery's own rows and
/// between descenders and a sentence, keys typed at the window and as the gallery types them:
/// no frame's foot lies past the row under the prompt, and no frame's box meets that row's
/// letters. Negative control (the law reverted alone, the cat, the gallery's rows, both ways of
/// typing): on each face, seated three rows under the prompt, it stands past the row under in
/// 40 frames or more and its box meets that row's letters in 10 or more (measured: 167 or 168
/// and 26 typed at the window, 98 and 29 to 31 as the gallery types).
#[test]
fn a_full_screen_program_s_rows_under_its_line_are_its_own_on_every_face() {
    let mut runs = Vec::new();
    for cell in CELLS {
        for rows in STEAM_ROWS {
            for style in STYLES {
                for ctl in [false, true] {
                    runs.push((cell, rows, style, ctl, false));
                }
            }
        }
        for ctl in [false, true] {
            runs.push((cell, STEAM_ROWS[0], STYLES[0], ctl, true));
        }
    }
    let seen = aterm_spec::check::each(&runs, |&(cell, rows, style, ctl, control)| {
        let old: &[OldPath] = if control {
            &[OldPath::FullScreenKeepsItsRow]
        } else {
            &[]
        };
        let frames = steamed(cell, style, rows, old, ctl, &[]);
        let drawn = frames
            .iter()
            .filter(|f| f.ms >= FROM && f.pet.is_some())
            .count();
        (
            drawn,
            past_the_row_under(&frames, cell.1),
            on_letters_of(&frames, PROMPT_ROW + 1).len(),
        )
    });
    for (&(cell, rows, style, ctl, control), &(drawn, past, on)) in runs.iter().zip(&seen) {
        assert!(
            drawn >= 200,
            "fixture: {style} on {cell:?} between {rows:?} (ctl {ctl}) is drawn ({drawn} frames)"
        );
        if control {
            assert!(
                past >= 40 && on >= 10,
                "negative control on {cell:?} (ctl {ctl}): seated under the line, the cat stood \
                 past the row under in {past} frames and met its letters in {on}"
            );
        } else {
            assert_eq!(
                (past, on),
                (0, 0),
                "{style} on {cell:?} between {rows:?} (ctl {ctl}): frames standing past the row \
                 under the prompt, frames whose box meets that row's letters"
            );
        }
    }
}

/// A BODY AT REST SETS OFF DRAWN FOR ITS WAY ([`OldPath::WayBeforeSettingOff`]), on every
/// face's cells ([`CELLS`]): on the alternate screen the cat comes to rest whole flush past the
/// row above's last letter (the gallery's row ends at its 52nd column, the cat seated at 52.4),
/// and sets off back into the line, a walk at the window's Backspaces, a gathered leap as the
/// gallery types. The head law reads the row above over that way while it is still at rest and
/// draws the body down or fits it before it goes. From 600 ms, the cat and the dog, both pages,
/// both ways of typing: no frame's box meets the row above's letters. Negative control (the law
/// reverted alone, the cat, both pages, both ways of typing): on each face its box meets that
/// row's last letters in two frames or more as it sets off (measured three to five, from the
/// first step or the take-off).
#[test]
fn a_body_at_rest_sets_off_drawn_for_its_way_on_every_face() {
    let mut runs = Vec::new();
    for cell in CELLS {
        for rows in STEAM_ROWS {
            for ctl in [false, true] {
                for style in STYLES {
                    runs.push((cell, rows, style, ctl, false));
                }
                runs.push((cell, rows, STYLES[0], ctl, true));
            }
        }
    }
    let seen = aterm_spec::check::each(&runs, |&(cell, rows, style, ctl, control)| {
        let old: &[OldPath] = if control {
            &[OldPath::WayBeforeSettingOff]
        } else {
            &[]
        };
        on_letters_of(&steamed(cell, style, rows, old, ctl, &[]), PROMPT_ROW - 1)
    });
    for (&(cell, rows, style, ctl, control), met) in runs.iter().zip(&seen) {
        if control {
            assert!(
                met.len() >= 2,
                "negative control on {cell:?} between {rows:?} (ctl {ctl}): read from its \
                 first step or its take-off, the cat's box met the row above's letters only in \
                 the frames at {met:?} ms"
            );
        } else {
            assert!(
                met.is_empty(),
                "{style} on {cell:?} between {rows:?} (ctl {ctl}): frames at {met:?} ms whose box \
                 meets the row above's letters"
            );
        }
    }
}

/// A run of the leaps' scene: the cells, the companion, whether keys come as the gallery types
/// them, the laws reverted, and how many frames a control's box meets the row above's letters in.
type LeapRun = (
    (usize, usize),
    &'static str,
    bool,
    &'static [OldPath],
    usize,
);

/// A BODY HELD, FITTED OR DRAWN DOWN IS DRAWN NO TALLER THAN ITS FIT ([`OldPath::HeldNoTaller`],
/// [`OldPath::DrawnDownNoTaller`]), on every face's cells ([`CELLS`]): on the alternate screen
/// under a row lettered past the seats along the prompt's row, over a row whose letters end
/// before the caret's column after the Backspaces, the program moves the caret five rows down
/// and back, three times down and twice up a second apart (vim's `5j` and `5k`), and the cat
/// leaps straight down from the prompt's row, fitted the first time and drawn down after, and
/// back up into it. From 600 ms, the cat and the dog, both ways of typing: no frame's box meets
/// the row above's letters, in the air or on the ground. Negative controls (the cat, typed at
/// the window), on each face: with [`OldPath::HeldNoTaller`] the stretch takes the box into
/// those letters in eight frames or more, over every leap and landing (measured 11 to 15); with
/// [`OldPath::DrawnDownNoTaller`] in two or more, at each touchdown back into the drawn-down row
/// (measured 2). A take-off down from that row is a
/// crossing off a row the body was held on, and the crossing law draws it no taller too
/// ([`OldPath::CrossingRisesIntoTheLine`], through `PetBrain::head_holds`): with both reverted
/// the box meets the letters at those take-offs as well, in four frames or more (measured 6, the
/// count [`OldPath::DrawnDownNoTaller`] alone took before the crossing law joined it).
#[test]
fn a_body_held_fitted_or_drawn_down_is_drawn_no_taller_than_its_fit_on_every_face() {
    let held = (
        "the quick brown fox jumps over the lazy dog, every glyph legible here",
        "Below: pqgyj",
    );
    let down: &[u8] = b"\x1b[10;15H";
    let up: &[u8] = b"\x1b[5;15H";
    let moves: &[(u64, &[u8])] = &[
        (300, down),
        (1_300, up),
        (2_300, down),
        (3_300, up),
        (4_300, down),
    ];
    let mut runs: Vec<LeapRun> = Vec::new();
    for cell in CELLS {
        for style in STYLES {
            for ctl in [false, true] {
                runs.push((cell, style, ctl, &[], 0));
            }
        }
        runs.push((cell, STYLES[0], false, &[OldPath::HeldNoTaller], 8));
        runs.push((cell, STYLES[0], false, &[OldPath::DrawnDownNoTaller], 2));
        runs.push((
            cell,
            STYLES[0],
            false,
            &[
                OldPath::DrawnDownNoTaller,
                OldPath::CrossingRisesIntoTheLine,
            ],
            4,
        ));
    }
    let seen = aterm_spec::check::each(&runs, |&(cell, style, ctl, old, _)| {
        let frames = steamed(cell, style, held, old, ctl, moves);
        (
            past_the_row_under(&frames, cell.1),
            on_letters_of(&frames, PROMPT_ROW - 1),
        )
    });
    for (&(cell, style, ctl, old, bites), (past, met)) in runs.iter().zip(&seen) {
        assert!(
            *past >= 150,
            "fixture: {style} on {cell:?} (ctl {ctl}) follows the caret down ({past} frames)"
        );
        if old.is_empty() {
            assert!(
                met.is_empty(),
                "{style} on {cell:?} (ctl {ctl}): frames at {met:?} ms whose box meets the row \
                 above's letters"
            );
        } else {
            assert!(
                met.len() >= bites,
                "negative control on {cell:?} ({old:?}): stretched past its fit, the box met the \
                 row above's letters only in the frames at {met:?} ms"
            );
        }
    }
}
