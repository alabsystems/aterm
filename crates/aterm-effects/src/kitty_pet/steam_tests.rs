// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The trail gallery's `steam` scene on the alternate screen, and its `last-row` on the main
//! screen's last row, replayed through the effects pipeline on fixed cells, so each law they pin
//! is judged on every face's geometry wherever the tests run (the render tests replay them on the
//! one face their platform bundles).

use aterm_core::render::RenderInput;
use aterm_core::terminal::{ClockReading, Rgb, Terminal};

use super::{OldPath, PetInputKind, ROW_FIT, art_cols};
use crate::cursor_fx::TrailColor;
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
/// The pets judged (`cursor_pet`): the cat, and the dog, each beside the rainbow.
const STYLES: [&str; 2] = ["cat", "dog"];
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

/// A Left arrow's key in a replay's events: the console reads it as a move along the line.
const LEFT: char = '\u{2190}';

/// A Right arrow's key in a replay's events: the console reads it as a move along the line.
const RIGHT: char = '\u{2192}';

/// A drawn frame: when, the companion's alpha on glass and its box (`x0, x1, y0, y1` px), the
/// lettered cells (`row, col`) the box meets by a pixel or more, and how far (rows) the head law
/// draws the body down its row and the share of its height it fits it to
/// ([`EffectsPipeline::companion_head_sink_fit`]), and the caret's cell (`row, col`).
#[derive(Clone, Debug)]
struct Frame {
    ms: u64,
    alpha: u8,
    pet: Option<(i32, i32, i32, i32)>,
    boxed: Vec<(usize, usize)>,
    sink_fit: (f32, f32),
    caret: (usize, usize),
}

/// `events` replayed on `cell` px cells, the pane `grid` rows by columns, a frame every
/// [`FRAME_MS`] to `tail` ms past the last, the laws in `old` reverted. A key's echo lands 2 ms
/// after it; typed at the window (`ctl` false) the console sees each key (a Backspace as a
/// delete, [`LEFT`] and [`RIGHT`] as moves along the line), as the gallery types it
/// (`aterm ctl key`) none.
fn replay(
    (cell, grid): ((usize, usize), (u16, u16)),
    style: &str,
    events: &[(u64, Ev)],
    tail: u64,
    old: &[OldPath],
    ctl: bool,
) -> Vec<Frame> {
    let (cw, ch) = cell;
    let mut term = Terminal::new(grid.0, grid.1);
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
        "rainbow",
        TrailColor::Automatic,
        None,
        260,
        24,
        1.0,
        0.6,
        true,
        0x0050_FA7B,
    );
    fx.set_cursor_pet(true, 0x5EED);
    fx.set_cursor_pet_species(crate::cursor_pet::species(style).expect("a pet"));
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
    let (rows, cols) = (usize::from(grid.0), usize::from(grid.1));
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
            (0, Ev::Key(LEFT | RIGHT, _)) => {
                fx.note_console_input(PetInputKind::Navigate);
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
                    alpha: fx.cursor_pet_alpha(),
                    pet,
                    boxed,
                    sink_fit: fx.companion_head_sink_fit(),
                    caret: (input.cursor_row, input.cursor_col),
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
fn steam_page(rows: (&str, &str)) -> String {
    steam_page_at(5, rows)
}

/// [`steam_page`] with the prompt on the pane's row `prompt`, counted from 1 (the `steam`
/// scene's is row 5, [`PROMPT_ROW`] counted from 0).
fn steam_page_at(prompt: u16, (above, below): (&str, &str)) -> String {
    format!(
        "\x1b[?1049h\x1b[2J\x1b[{}H{above}\x1b[{}H{below}\x1b[{prompt}H$ ",
        prompt - 1,
        prompt + 1
    )
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
    replay((cell, (ROWS, COLS)), style, &ev, 1_600, old, ctl)
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

/// The trail gallery's `last-row` page's full row, cycled to the grid's width: a letter in every
/// cell.
const LAST_ROW_FULL: &str = "Typography_jumps;quick_brown_gyps(qwerty)_pqgyj_{braces}[ok]|";
/// The rows `last-row`'s page lays between its full ones.
const LAST_ROW_RAGGED: [&str; 4] = [
    "fn main() {",
    "    let fox = \"the quick brown fox\";",
    "    // every glyph here must stay legible: pqgyj {braces} [ok]",
    "}",
];
/// The burst `last-row` types along the grid's last row, after `$ `.
const BURST_TEXT: &str = "echo the quick brown fox jumps over the lazy dog as the kitty flies";
/// [`Then::Gapped`]'s burst: [`BURST_TEXT`]'s length, its last words a run of fourteen spaces and
/// two letters, so the line's last letters lie by the margin and a stand by the caret off them
/// lies in the spaces before them.
const GAPPED_TEXT: &str = "echo the quick brown fox jumps over the lazy dog as              ab";
const _: () = assert!(GAPPED_TEXT.len() == BURST_TEXT.len());
/// How long (ms) the burst's line rests by the margin before its Enter.
const LAST_ROW_REST: u64 = 3_000;
/// The blank cells past the burst's line the wall is judged at, on each face's cells: from more
/// than the fitted body takes on every face to none, and `-1`, a line a cell too long for its row,
/// which wraps.
const LAST_ROW_ROOMS: [i16; 6] = [4, 3, 2, 1, 0, -1];

/// What `last-row`'s line does once its burst is typed, before its Enter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Then {
    /// It rests [`LAST_ROW_REST`].
    Rests,
    /// `keys` Backspaces 50 ms apart from `after` ms past the last key, each echoed as a line
    /// editor echoes one at its line's end (back a cell, erase to the end), the Enter 4 s past
    /// the last key.
    Erased { keys: u16, after: u64 },
    /// Ten Left arrows 50 ms apart from 1.5 s past the last key (echoed back a cell), the Enter
    /// 5 s past the last key.
    Back,
    /// The burst typed to [`PAUSED_SHORT`] keys short of its end, a 3 s pause, then the rest,
    /// and [`LAST_ROW_REST`].
    Paused,
    /// 1.5 s past the last key the program clears the row over the line ([`OPEN_ROW_OVER`]),
    /// the caret kept where it stands: a seat opens beside the line while the body is out. The
    /// Enter 4 s past the last key.
    Opened,
    /// [`GAPPED_TEXT`] typed for the burst, and [`LAST_ROW_REST`].
    Gapped,
}

/// The bytes [`Then::Opened`]'s program writes: the caret saved, a row up, that row cleared,
/// the caret restored.
const OPEN_ROW_OVER: &[u8] = b"\x1b7\x1b[A\x1b[2K\x1b8";

/// How many keys short of its end [`Then::Paused`]'s burst pauses.
const PAUSED_SHORT: usize = 6;

/// A `last-row` replay: its frames, its last key (the wrapping one where the line wraps), its
/// Enter (ms), and the first key past the burst's line (a Backspace's, a Left arrow's) or, for
/// [`Then::Paused`], the first key past the pause.
struct LastRow {
    frames: Vec<Frame>,
    last_key: u64,
    enter: u64,
    then_from: Option<u64>,
}

/// The trail gallery's `last-row` page on a grid `cols` wide, [`ROWS`] rows tall: the screen
/// cleared, each row over the last a letter in every cell ([`LAST_ROW_FULL`], the row over the
/// last among them) or [`LAST_ROW_RAGGED`]'s, by turns, and on the last the prompt `$ `, after the
/// first `path` cells of [`LAST_ROW_PATH`] over and over.
fn last_row_page(cols: u16, path: usize) -> String {
    let mut page = String::from("\x1b[H\x1b[2J");
    for over in (1..usize::from(ROWS)).rev() {
        let text: String = if over % 2 == 1 {
            LAST_ROW_FULL
                .chars()
                .cycle()
                .take(usize::from(cols))
                .collect()
        } else {
            LAST_ROW_RAGGED[over / 2 % LAST_ROW_RAGGED.len()].to_string()
        };
        page.push_str(&text);
        page.push_str("\r\n");
    }
    page.extend(LAST_ROW_PATH.chars().cycle().take(path));
    page.push_str("$ ");
    page
}

/// A working directory a prompt names before its `$ `, so the line runs on to a wider grid's end
/// ([`last_row_page`]).
const LAST_ROW_PATH: &str = "~/src/aterm/crates/aterm-effects/src/kitty_pet/";
/// How many cells longer [`a_wall_that_holds_no_seat_hides_the_body_on_every_face`]'s second
/// width runs the prompt ([`last_row_after`]): the grid 103 to 108 columns wide, where a box flush
/// with the wall once ended a rounding past the grid's last column on 13 and 14 px cells
/// ([`PetBrain::rest_box_cols`]).
const WALL_FLUSH_PATH: usize = 35;

/// The trail gallery's `last-row` on `cell` px cells, the grid as wide as leaves `room` blank
/// cells past the burst's line on the main screen's last row: the page's rows over it (the row
/// over the line a letter in every cell), the burst typed from [`TYPING_FROM`] a key every 50 ms,
/// what `then` says, then an Enter whose answer scrolls the page and prints a new prompt.
fn last_row(
    cell: (usize, usize),
    scene: (i16, Then),
    style: &str,
    old: &[OldPath],
    ctl: bool,
) -> LastRow {
    last_row_after(cell, 0, scene, style, old, ctl)
}

/// [`last_row`] with the prompt `path` cells longer ([`last_row_page`]), the grid with it.
fn last_row_after(
    cell: (usize, usize),
    path: usize,
    (room, then): (i16, Then),
    style: &str,
    old: &[OldPath],
    ctl: bool,
) -> LastRow {
    let line = i16::try_from(path + "$ ".len() + BURST_TEXT.len()).expect("a line a grid holds");
    let cols = u16::try_from(line + room).expect("a grid as wide as the line");
    let mut ev = vec![(200, Ev::Out(last_row_page(cols, path).into_bytes()))];
    let (last_key, then_from) = if then == Then::Paused {
        let short = BURST_TEXT.len() - PAUSED_SHORT;
        ev.extend(typed(&BURST_TEXT[..short], TYPING_FROM, 50));
        let resume = TYPING_FROM + 50 * short as u64 + 3_000;
        ev.extend(typed(&BURST_TEXT[short..], resume, 50));
        (resume + 50 * (PAUSED_SHORT as u64 - 1), Some(resume))
    } else {
        let burst = if then == Then::Gapped {
            GAPPED_TEXT
        } else {
            BURST_TEXT
        };
        ev.extend(typed(burst, TYPING_FROM, 50));
        let last_key = TYPING_FROM + 50 * (burst.len() as u64 - 1);
        let (keys, from, key, echo): (u16, u64, char, &[u8]) = match then {
            Then::Erased { keys, after } => (keys, after, '\u{7f}', b"\x08\x1b[K"),
            Then::Back => (10, 1_500, LEFT, b"\x08"),
            Then::Rests | Then::Paused | Then::Opened | Then::Gapped => (0, 0, ' ', b""),
        };
        ev.extend(
            (0..u64::from(keys)).map(|k| (last_key + from + 50 * k, Ev::Key(key, echo.to_vec()))),
        );
        if then == Then::Opened {
            ev.push((last_key + 1_500, Ev::Out(OPEN_ROW_OVER.to_vec())));
            (last_key, Some(last_key + 1_500))
        } else {
            (last_key, (keys > 0).then_some(last_key + from))
        }
    };
    let enter = last_key
        + match then {
            Then::Rests | Then::Paused | Then::Gapped => LAST_ROW_REST,
            Then::Erased { .. } | Then::Opened => 4_000,
            Then::Back => 5_000,
        };
    ev.push((enter, Ev::Key('\r', b"\r\n".to_vec())));
    ev.push((enter + 6, Ev::Out(b"Below: pqgyj [ok]\r\n$ ".to_vec())));
    ev.sort_by_key(|e| e.0);
    LastRow {
        frames: replay((cell, (ROWS, cols)), style, &ev, 1_600, old, ctl),
        last_key,
        enter,
        then_from,
    }
}

/// What a `last-row` run shows, from 300 ms past the first key to its end.
#[derive(Clone, Debug)]
struct WallSeen {
    /// Frames to the Enter (wrapped: to the wrap) on glass whose box meets a lettered cell.
    on: usize,
    /// Frames to the Enter (wrapped: to the wrap) on glass.
    drawn: usize,
    /// Frames to the Enter (wrapped: to the wrap).
    held: usize,
    /// The most px a box on glass reaches into a lettered cell, to the Enter (wrapped: the wrap).
    reach: i32,
    /// The first frame (ms) on glass whose box meets a lettered cell.
    first: Option<u64>,
    /// The first frame (ms) from `first` off the glass.
    gone: Option<u64>,
    /// Frames (ms) from `first` to `gone` whose alpha rose over the frame before's.
    rose: Vec<u64>,
    /// Frames from `first` to `gone` on glass below full alpha whose box meets a lettered cell:
    /// the wall's fade over the letters.
    faded_on: usize,
    /// Frames (ms) on glass from `gone` to the Enter (not judged where the line wraps).
    unfaded: Vec<u64>,
    /// The first frame (ms) on glass from `gone` to the Enter: the body back before it.
    returned: Option<u64>,
    /// Frames (ms) on glass from `gone` to the Enter whose box reaches more than 2 px into a
    /// lettered cell.
    relaid: Vec<u64>,
    /// Frames on glass from the first key past the burst's line ([`LastRow::then_from`]) to the
    /// Enter whose box meets a lettered cell.
    then_on: usize,
    /// Of those, the frames at full alpha whose box reaches more than 2 px into a lettered cell.
    full: usize,
    /// Frames (ms) on glass past the Enter (wrapped: past the wrap) whose box meets a lettered
    /// cell.
    after: Vec<u64>,
    /// The first frame (ms) on glass past the Enter (wrapped: the wrap) and past `gone`.
    back: Option<u64>,
    /// The Enter (wrapped: the wrap), ms.
    until: u64,
}

/// The most px `f`'s box reaches into a lettered cell on cells `cw` px wide (0: none).
fn reach_of(f: &Frame, cw: usize) -> i32 {
    match f.pet {
        Some((x0, x1, ..)) if f.alpha > 0 => f
            .boxed
            .iter()
            .map(|&(_, c)| {
                let (c0, c1) = ((c * cw) as i32, ((c + 1) * cw) as i32);
                x1.min(c1) - x0.max(c0)
            })
            .max()
            .unwrap_or(0),
        _ => 0,
    }
}

/// A `last-row` run's sum ([`WallSeen`]) on cells `cw` px wide: `wraps`, the line wraps at its
/// last key.
fn wall_seen(run: &LastRow, wraps: bool, cw: usize) -> WallSeen {
    let from = TYPING_FROM + 300;
    let until = if wraps { run.last_key } else { run.enter };
    let lettered = |f: &Frame| f.alpha > 0 && !f.boxed.is_empty();
    let held: Vec<&Frame> = run
        .frames
        .iter()
        .filter(|f| f.ms >= from && f.ms < until)
        .collect();
    let reach = held.iter().map(|f| reach_of(f, cw)).max().unwrap_or(0);
    let first = held.iter().find(|f| lettered(f)).map(|f| f.ms);
    let gone = first.and_then(|first| {
        run.frames
            .iter()
            .find(|f| f.ms >= first && f.alpha == 0)
            .map(|f| f.ms)
    });
    let rose = match (first, gone) {
        (Some(first), Some(gone)) => run
            .frames
            .windows(2)
            .filter(|w| w[0].ms >= first && w[1].ms <= gone && w[1].alpha > w[0].alpha)
            .map(|w| w[1].ms)
            .collect(),
        _ => Vec::new(),
    };
    let faded_on = match (first, gone) {
        (Some(first), Some(gone)) => run
            .frames
            .iter()
            .filter(|f| f.ms >= first && f.ms < gone && f.alpha < u8::MAX && lettered(f))
            .count(),
        _ => 0,
    };
    let rested: Vec<&&Frame> = match gone {
        Some(gone) if !wraps => held.iter().filter(|f| f.ms >= gone).collect(),
        _ => Vec::new(),
    };
    let past: Vec<&Frame> = run.frames.iter().filter(|f| f.ms > until).collect();
    WallSeen {
        on: held.iter().filter(|f| lettered(f)).count(),
        drawn: held.iter().filter(|f| f.alpha > 0).count(),
        held: held.len(),
        reach,
        first,
        gone,
        rose,
        faded_on,
        unfaded: rested
            .iter()
            .filter(|f| f.alpha > 0)
            .map(|f| f.ms)
            .collect(),
        returned: rested.iter().find(|f| f.alpha > 0).map(|f| f.ms),
        relaid: rested
            .iter()
            .filter(|f| reach_of(f, cw) > 2)
            .map(|f| f.ms)
            .collect(),
        then_on: run.then_from.map_or(0, |t| {
            held.iter().filter(|f| f.ms >= t && lettered(f)).count()
        }),
        full: run.then_from.map_or(0, |t| {
            held.iter()
                .filter(|f| f.ms >= t && f.alpha == u8::MAX && reach_of(f, cw) > 2)
                .count()
        }),
        after: past.iter().filter(|f| lettered(f)).map(|f| f.ms).collect(),
        back: past
            .iter()
            .find(|f| f.alpha > 0 && gone.is_none_or(|g| f.ms > g))
            .map(|f| f.ms),
        until,
    }
}

/// The blank cells past the burst's line on a grid `room` cells wider than it once `keys`
/// Backspaces have erased it ([`Then::Erased`]): a cell each, and two for the first where the
/// line ran to the grid's end, since the caret stood on its last letter there, the back-step
/// leaves that letter after it, and the erase to the line's end takes it.
fn erased_room(room: i16, keys: u16) -> i16 {
    room + keys as i16 + i16::from(room == 0 && keys > 0)
}

/// What [`a_wall_that_holds_no_seat_hides_the_body_on_every_face`] replays past the burst's line
/// where the line leaves the body no seat: the rest, an erase after the fade (3, 6 and 12
/// Backspaces), one during it, Left arrows back into the line, a pause before its last keys
/// (replayed where the line seats the body too), the row over the line cleared, and a line whose
/// last letters follow a run of spaces (replayed where it seats the body too).
const WALL_THENS: [Then; 9] = [
    Then::Rests,
    Then::Erased {
        keys: 3,
        after: 1_500,
    },
    Then::Erased {
        keys: 6,
        after: 1_500,
    },
    Then::Erased {
        keys: 12,
        after: 1_500,
    },
    Then::Erased {
        keys: 6,
        after: 150,
    },
    Then::Back,
    Then::Paused,
    Then::Opened,
    Then::Gapped,
];

/// A run of the wall's scene: the cells and how many cells longer the prompt runs
/// ([`last_row_after`]), the blank cells past the line and what follows the burst, the companion,
/// whether keys come as the gallery types them, and the law its negative control reverts, if it
/// is one.
type WallRun = (
    ((usize, usize), usize),
    (i16, Then),
    &'static str,
    bool,
    Option<OldPath>,
);

/// THE WALL THAT HOLDS NO SEAT HIDES THE BODY ([`OldPath::NoSeatHidesTheBody`]), on every face's
/// cells ([`CELLS`]): the trail gallery's `last-row` on the main screen's last row, the row over it
/// lettered in every cell and none under, the burst's line ending 4, 3, 2, 1 and 0 cells short of
/// the grid's end, and one cell too long for its row, so that it wraps ([`LAST_ROW_ROOMS`]). The
/// cat and the dog, keys typed at the window and as the gallery types them, from 300 ms past the
/// first key. Where the line leaves the room the body fitted to its row takes (`art_cols` x
/// [`ROW_FIT`] cells: 3.38 on 13 x 29, 2.82 on 14 x 26, 3.86 on 11 x 28), typed to its end at once,
/// paused [`PAUSED_SHORT`] keys short of it ([`Then::Paused`]), its last letters after a run of
/// spaces ([`Then::Gapped`]) or walked back into by ten Left arrows ([`Then::Back`]: the body
/// keeps its seat off the line, [`OldPath::KeptOffTheLine`]), it is on glass in every frame to the
/// Enter (the caret's cell is not judged here: on 14 x 26 the box overlaps it, though rendered on
/// that cell its art lay there in no frame, `companion_off_letters`), and its box meets
/// no lettered cell but by a pose's swell at the wall, which the grid's end pushes back onto the
/// line ([`OldPath::StretchOffLetters`] leaves it so), 2 px at most (measured on 11 x 28 at four
/// cells: 1 px, in 3 frames, in 7 past the pause and in 13 from the first arrow; none on the
/// others). Where the line leaves less, a frame's box meets its last letters as they come, and
/// from that frame the body fades out ([`super::FADE_OUT`]), drawn off the glass within the
/// wall's cut of it ([`super::FADE_OFF_THE_LETTERS`], 64 ms; [`OldPath::LettersCutTheFade`]): its
/// alpha never rises, its box lies on the letters below full alpha in 3 frames at most (the cut's
/// 4 frames at 60 Hz, the 4th off the glass; measured 3 in every run, at alphas 184, 118 and 57),
/// and it is off the glass the cut and a frame on (measured 48 ms, and 64 where the first frame
/// on the letters comes at full alpha, a tick before the wall's law hides it: past the pause at no
/// blank cell and at one on 13 x 29 and 11 x 28, and at none on 14 x 26). Past the Enter, and
/// where the line wraps past the wrap, it is back on glass within a second ([`super::FADE_IN`])
/// and no frame's box meets a lettered cell: it comes back by the new caret, and never crosses
/// the wrapped line's first row.
///
/// Where the line leaves no seat, what follows it before the Enter ([`WALL_THENS`]):
/// - It rests: off the glass from its fade's end to the Enter.
/// - 3, 6 or 12 Backspaces 1.5 s past the last key, or 6 from 150 ms, while its fade runs on
///   (off the glass by then): where the erase leaves the fitted body its room ([`erased_room`]),
///   it is back on glass within a second of the erase's last key (or of its fade's end), and where
///   it leaves less, off the glass to the Enter. Either way no frame from its fade's end to the
///   Enter lays its box more than 2 px into a lettered cell (the same swell, 1 px in 3 frames on
///   11 x 28 at four cells).
/// - Ten Left arrows back into the line 1.5 s past the last key: off the glass from its fade's
///   end to the Enter, no stand by the caret being off the letters.
/// - The burst paused [`PAUSED_SHORT`] keys short of its end for 3 s: off the glass from its
///   fade's end to the Enter.
/// - 1.5 s past the last key the program clears the row over the line ([`Then::Opened`]), so a
///   seat opens beside the line while the body is out: back on glass within a second of the
///   clearing (or of its fade's end; measured 24 ms past the clearing on every face), and no
///   frame from its fade's end to the Enter lays its box more than 2 px into a lettered cell.
///   Past the Enter, the frame before it moves has the Enter's scroll carry the line up under
///   that seat (the same at four blank cells, which seat the body, and with the law reverted):
///   that frame is the scroll's, and is not judged.
/// - The burst's last two letters typed after fourteen spaces ([`Then::Gapped`]), so that a stand
///   by the caret off the letters lies in the spaces: off the glass from its fade's end to the
///   Enter, the wall holding it no seat on every tick there (stood at that stand, the body blinked
///   back and out every 64 ms from its fade's end to the Enter on every face), and where the line
///   seats it, on glass in every frame to the Enter (replayed there too).
///
/// In each, from the first key past the line (past the pause) to the Enter, at full alpha its
/// box reaches more than 2 px into a lettered cell in one frame at most (measured: one past the
/// pause at one blank cell and at none on 13 x 29 and 11 x 28, at none on 14 x 26; none in the
/// others).
///
/// Every run but the negative controls is replayed twice: on the gallery's grid, and with the
/// prompt [`WALL_FLUSH_PATH`] cells longer, on 103 to 108 columns, where a box flush with the
/// wall once read a column past the grid's end ([`PetBrain::rest_box_cols`]). There the law held
/// nothing on 14 x 26: the body stood on the line's last letters through the rest in 188 to 194
/// frames, and from the first Left arrow at a seat followed the caret onto them in 213 and 215
/// (Linux arm64, 2026-11-08).
///
/// Negative control (the law reverted, with [`OldPath::CaretCellIsNoSeat`], the cat, every face,
/// both ways of typing, every room that leaves no seat): it stands at the wall over the line's last
/// letters to the Enter, its box on them in 126 frames or more, two thirds of the fewest measured (188 to 197 of the 375 held; 188
/// to 191 past the gap, each at full alpha). Where
/// the line wraps, as the gallery types it, its crossing back after the wrap lays its box on the
/// wrapped line's letters in 21 frames or more, two thirds of those measured (31 on every face);
/// typed at the window the reverted crossing keeps off them (measured 0), so that control is the
/// gallery's typing alone. From the first Left arrow to the Enter it follows the caret along the
/// line's letters, its box on them in 146 frames or more (measured 219 of 219); from the end of
/// the pause, in 125 or more (187 to 197 of 203). The erase after the line ran to the grid's end
/// leaves it on the letters from the first Backspace to the Enter in 18 frames or more (measured
/// 26 to 240: 156 with three, 45 to 156 with six, 26 to 35 with twelve, 44 to 240 with six while
/// it fades; at the other rooms the reverted body is clear of the letters within 8 frames of the
/// erase, and no control is taken there). The caret's cell is no seat either
/// ([`PetBrain::wall_meets_the_caret`]): with the hide reverted alone, that law takes a body whose
/// box at the wall lies on the caret's cell off to a seat off the line once the hand's rhythm closes,
/// and the box met the letters only through that wait (54 frames on 13 x 29 at three blank cells,
/// typed at the window, macOS, 2026-11-10), so this control reverts both: it is what the wall did
/// before either law.
///
/// Negative control of the cut ([`OldPath::LettersCutTheFade`] reverted, the cat, every face, both
/// ways of typing, the rest at every room that leaves no seat): the body lies on the letters for
/// its whole fade, its box on them below full alpha in 19 frames or more, two thirds of the fewest
/// measured (28 at every room that holds the line; 12 to the wrap on 13 x 29 and 11 x 28 and 9 on
/// 14 x 26, so 6 or more there; Linux arm64, 2026-11-09).
///
/// Negative control of the seat kept off the line ([`OldPath::KeptOffTheLine`] reverted, the cat,
/// every face, both ways of typing, the Left arrows at every room that seats the body): it
/// follows the caret onto the line's letters, its box on them from the first arrow to the Enter
/// in 142 frames or more, two thirds of the fewest measured (213 to 219 of 219, 198 to 204 of
/// those at full alpha, reaching 11 to 14 px into them; Linux arm64, 2026-11-08).
///
/// Negative control of the caret's own steps ([`OldPath::CaretTypesOn`] reverted, the cat, every
/// face, typed as the gallery types them, the pause at every room but the wrap): the keys typed
/// on after the pause, short of the rhythm's [`super::RHYTHM_MOVES`], find the margin untaken,
/// and the body hops over the caret onto the line's last letters at full alpha. Over the rooms
/// its box reaches more than 2 px into them at full alpha in 15 frames or more, two thirds of
/// the fewest measured (22 on 13 x 29: 10 at three blank cells, 10 at two, 1 at one and 1 at
/// none; 24 on 14 x 26: 11 at two, 12 at one, 1 at none; 40 on 11 x 28: 30 at four, which seat
/// the body, 8 at three, none at two, 1 at one and 1 at none), against 2, 1 and 2 with the law.
/// Linux arm64, 2026-11-07.
#[test]
fn a_wall_that_holds_no_seat_hides_the_body_on_every_face() {
    let mut runs: Vec<WallRun> = Vec::new();
    for cell in CELLS {
        let fitted = art_cols(cell.0 as u16, cell.1 as u16) * ROW_FIT;
        for room in LAST_ROW_ROOMS {
            for then in WALL_THENS {
                let unseated = f32::from(room) < fitted;
                let judged = match then {
                    Then::Rests => true,
                    Then::Paused | Then::Gapped | Then::Back => room >= 0,
                    _ => room >= 0 && unseated,
                };
                if !judged {
                    continue;
                }
                for ctl in [false, true] {
                    for path in [0, WALL_FLUSH_PATH] {
                        for style in STYLES {
                            runs.push(((cell, path), (room, then), style, ctl, None));
                        }
                    }
                    let cell = (cell, 0);
                    let control = match then {
                        Then::Rests => room >= 0 || ctl,
                        Then::Erased { .. } => room == 0,
                        Then::Back | Then::Paused | Then::Gapped => true,
                        Then::Opened => false,
                    };
                    if unseated && control {
                        let old = Some(OldPath::NoSeatHidesTheBody);
                        runs.push((cell, (room, then), STYLES[0], ctl, old));
                    }
                    if !unseated && then == Then::Back {
                        let old = Some(OldPath::KeptOffTheLine);
                        runs.push((cell, (room, then), STYLES[0], ctl, old));
                    }
                    if then == Then::Paused && ctl {
                        let old = Some(OldPath::CaretTypesOn);
                        runs.push((cell, (room, then), STYLES[0], ctl, old));
                    }
                    if unseated && then == Then::Rests {
                        let old = Some(OldPath::LettersCutTheFade);
                        runs.push((cell, (room, then), STYLES[0], ctl, old));
                    }
                }
            }
        }
    }
    let seen = aterm_spec::check::each(&runs, |&((cell, path), scene, style, ctl, control)| {
        // The hide's control reverts the caret's cell's law with it (the doc above).
        let old: Vec<OldPath> = match control {
            Some(OldPath::NoSeatHidesTheBody) => {
                vec![OldPath::NoSeatHidesTheBody, OldPath::CaretCellIsNoSeat]
            }
            _ => control.into_iter().collect(),
        };
        let run = last_row_after(cell, path, scene, style, &old, ctl);
        (wall_seen(&run, scene.0 < 0, cell.0), run.last_key)
    });
    for cell in CELLS {
        let full: Vec<usize> = runs
            .iter()
            .zip(&seen)
            .filter(|(run, _)| run.0.0 == cell && run.4 == Some(OldPath::CaretTypesOn))
            .map(|(_, (s, _))| s.full)
            .collect();
        assert!(
            full.iter().sum::<usize>() >= 15,
            "negative control on {cell:?} ({:?}): typed on past the console after the pause, \
             its box met the letters at full alpha in only {full:?} frames",
            OldPath::CaretTypesOn
        );
    }
    for (&((cell, path), (room, then), style, ctl, control), (s, last_key)) in
        runs.iter().zip(&seen)
    {
        let why = format!(
            "{style} on {cell:?}, the prompt {path} cells longer, {room} cells past the line, \
             {then:?} (ctl {ctl})"
        );
        let fitted = art_cols(cell.0 as u16, cell.1 as u16) * ROW_FIT;
        assert!(s.held >= 150, "fixture: {why} holds {} frames", s.held);
        if control == Some(OldPath::CaretTypesOn) {
            continue;
        }
        if control == Some(OldPath::LettersCutTheFade) {
            let bites = if room < 0 { 6 } else { 19 };
            assert!(
                s.faded_on >= bites,
                "negative control, {why}: the cut reverted, its fade lay on the letters in only \
                 {} frames ({s:?})",
                s.faded_on
            );
            continue;
        }
        if control == Some(OldPath::KeptOffTheLine) {
            assert!(
                s.then_on >= 142,
                "negative control, {why}: seated, from the first Left arrow its box met the \
                 letters in only {} frames ({s:?})",
                s.then_on
            );
            continue;
        }
        if control.is_some() {
            let (met, bites) = match then {
                Then::Rests if room < 0 => (s.after.len(), 21),
                Then::Rests | Then::Gapped => (s.on, 126),
                Then::Erased { .. } => (s.then_on, 18),
                Then::Back => (s.then_on, 146),
                Then::Paused => (s.then_on, 125),
                Then::Opened => unreachable!("no control is taken where a seat opens"),
            };
            assert!(
                met >= bites,
                "negative control, {why}: with no seat, its box met the letters in only {met} \
                 frames ({s:?})"
            );
            continue;
        }
        if f32::from(room) >= fitted {
            assert!(
                s.drawn == s.held && s.reach <= 2,
                "{why}: seated, the body left the glass or its box reached {} px into a letter's \
                 cell ({s:?})",
                s.reach
            );
            continue;
        }
        let first = s
            .first
            .unwrap_or_else(|| panic!("fixture: {why}: the line never met the box ({s:?})"));
        let gone = s
            .gone
            .unwrap_or_else(|| panic!("{why}: the body never left the glass ({s:?})"));
        // The wall's cut (`FADE_OFF_THE_LETTERS`, 64 ms: 4 frames, the 4th off the glass) and a
        // frame.
        assert!(
            s.rose.is_empty() && gone <= first + 64 + FRAME_MS,
            "{why}: from the box's meeting the letters at {first} ms its fade rose at {:?}, or \
             took to {gone} ms",
            s.rose
        );
        assert!(
            s.faded_on <= 3,
            "{why}: its fade lay on the letters in {} frames ({s:?})",
            s.faded_on
        );
        assert!(
            s.full <= 1,
            "{why}: past the line, its box met the letters at full alpha in {} frames ({s:?})",
            s.full
        );
        assert!(
            s.relaid.is_empty(),
            "{why}: frames on glass after its fade whose box lies on a letter: {:?}",
            s.relaid
        );
        match then {
            Then::Erased { keys, after } if f32::from(erased_room(room, keys)) >= fitted => {
                let erased = last_key + after + 50 * u64::from(keys - 1);
                assert!(
                    s.returned.is_some_and(|r| r <= erased.max(gone) + 1_000),
                    "{why}: the erase left it its room and it was not back on glass a second \
                     past it ({s:?})"
                );
            }
            Then::Opened => {
                let opened = last_key + 1_500;
                assert!(
                    s.returned.is_some_and(|r| r <= opened.max(gone) + 1_000),
                    "{why}: a seat opened beside the line and it was not back on glass a second \
                     past it ({s:?})"
                );
            }
            _ => assert!(
                s.unfaded.is_empty(),
                "{why}: frames on glass after its fade, the line under it: {:?}",
                s.unfaded
            ),
        }
        assert!(
            s.back.is_some_and(|b| b <= s.until.max(gone) + 1_000),
            "{why}: not back on glass a second past the wrap or the Enter ({s:?})"
        );
        // Off the line on the row the seat opened over it, the body has the Enter's scroll carry
        // the line up under it for the frame before it moves (measured the same at four blank
        // cells, which seat it, and with the law reverted): that frame is the scroll's, not the
        // wall's.
        let scrolled_under = |ms: u64| then == Then::Opened && ms <= s.until + FRAME_MS;
        assert!(
            s.after.iter().all(|&ms| scrolled_under(ms)),
            "{why}: frames past the wrap or the Enter whose box meets a letter: {:?}",
            s.after
        );
    }
}

/// A BODY SLID UP OFF THE LINE GROWS ONCE IT HAS RISEN TO ITS ROW
/// ([`OldPath::FitWaitsForTheFeet`]), on every face's cells ([`CELLS`]): the trail gallery's
/// `last-row` where the line leaves the body fitted to its row a seat at the wall (`art_cols` x
/// [`ROW_FIT`] cells past it or more: 4, 5 and 6 blank cells, and 3 on 14 x 26), and 1.5 s past
/// the last key the program clears the row over the line ([`Then::Opened`]). The margin's seat off
/// the line stands the body on the cleared row in one tick, drawn where it stood, its feet in the
/// line's row, and the head law draws it whole there, on that row (no sink). From the clearing to
/// the Enter, the cat and the dog, keys typed at the window and as the gallery types them: it is
/// on glass in every frame, and no frame's box meets a lettered cell. Negative control (the law
/// reverted alone, the cat, each way of typing): it grows as it rises, the wall holding its right
/// edge, and its box meets the line's last letters in 8 frames or more over each face's rooms,
/// two thirds of the fewest measured (11 on 13 x 29: 6, 4 and 1 at four, five and six blank
/// cells; 12 on 14 x 26: 7, 4, 1 and none at three to six; 15 on 11 x 28: 7, 5 and 3; the same
/// both ways of typing; Linux arm64, 2026-11-08).
#[test]
fn a_body_slid_up_off_the_line_grows_once_it_has_risen_to_its_row_on_every_face() {
    let mut runs = Vec::new();
    for cell in CELLS {
        let fitted = art_cols(cell.0 as u16, cell.1 as u16) * ROW_FIT;
        for room in [3i16, 4, 5, 6] {
            if f32::from(room) < fitted {
                continue;
            }
            for ctl in [false, true] {
                for style in STYLES {
                    runs.push((cell, room, style, ctl, false));
                }
                runs.push((cell, room, STYLES[0], ctl, true));
            }
        }
    }
    let seen = aterm_spec::check::each(&runs, |&(cell, room, style, ctl, control)| {
        let old: &[OldPath] = if control {
            &[OldPath::FitWaitsForTheFeet]
        } else {
            &[]
        };
        wall_seen(
            &last_row(cell, (room, Then::Opened), style, old, ctl),
            false,
            cell.0,
        )
    });
    let mut bitten = std::collections::BTreeMap::new();
    for (&(cell, room, style, ctl, control), s) in runs.iter().zip(&seen) {
        let why = format!("{style} on {cell:?}, {room} cells past the line (ctl {ctl})");
        assert!(s.held >= 150, "fixture: {why} holds {} frames", s.held);
        if control {
            *bitten.entry((cell, ctl)).or_insert(0) += s.then_on;
            continue;
        }
        assert!(
            s.drawn == s.held && s.then_on == 0,
            "{why}: from the clearing over the line, off the glass in {} frames or its box on a \
             letter in {} ({s:?})",
            s.held - s.drawn,
            s.then_on
        );
    }
    for ((cell, ctl), met) in bitten {
        assert!(
            met >= 8,
            "negative control on {cell:?} (ctl {ctl}, {:?}): grown as it rose, its box met the \
             line's letters in only {met} frames",
            OldPath::FitWaitsForTheFeet
        );
    }
}

/// How many rows tall the pane of [`FAR_LEAPS`]' tall scenes is: past the rows the console's map
/// reads at once ([`crate::pet_world::MAX_WORLD_ROWS`]).
const TALL_ROWS: u16 = 100;

/// How many columns wide the pane of [`FAR_LEAPS`]' wide scenes is: past the columns the
/// console's map reads at once ([`crate::pet_world::MAX_WORLD_COLS`]).
const WIDE_COLS: u16 = 300;

/// How many columns wide the pane of [`FAR_LEAPS`]' widest scene is: past the columns the
/// caret's window and the window about the body read together
/// (2 x [`crate::pet_world::MAX_WORLD_COLS`]), so a way from one to the other crosses columns
/// neither reads.
const WIDEST_COLS: u16 = 1_000;

/// A scene of [`a_body_far_from_the_caret_reads_its_own_rows_on_every_face`]: the caret leaps
/// far from the body and back, twice.
#[derive(Clone, Copy, Debug)]
struct FarLeap {
    /// What it is.
    name: &'static str,
    /// The pane, rows by columns.
    grid: (u16, u16),
    /// Whether the caret leaps after the `steam` scene's typing and Backspaces ([`steamed`]'s,
    /// both ways of typing), or from rest, the page's first frame on, typing no key: there the
    /// two ways of typing are the same replay, which runs once.
    typed: bool,
    /// Where the caret rests on the prompt's row (the page's, [`steam_page_at`]) and where it
    /// leaps to (rows and columns, counted from 1): from rest the caret goes to `from` 300 ms
    /// after the page, to `to` 1.2 s later, and back, to and back again 1.2 s apart; after the
    /// Backspaces it goes to `to` 300 ms after the last, back 1.2 s later, and to and back again
    /// 1.2 s apart.
    from: (u16, u16),
    to: (u16, u16),
    /// How many frames, at the fewest, the negative control's box meets the letters its law
    /// keeps it off ([`kept_off`] for [`FAR_LEAPS`], the row under's for [`TURNED_LEAPS`])
    /// between rows lettered to the margin, typed at the window and as the gallery types (a
    /// scene from rest: the first; for [`ARC_LEAPS`], over all of a scene's controls).
    bites: [usize; 2],
}

/// The leaps far from the body ([`FarLeap`]), each control's floor two thirds of the fewest
/// frames it was measured at on Linux arm64, 2026-11-07 (cat and dog, 13 x 29, 14 x 26 and
/// 11 x 28 px cells): from rest 38, 43 and 12 frames; after the Backspaces 9 at the window
/// and 11 as the gallery types, on the tall pane, 37 and 39 on the wide one; from rest on a
/// 160-row pane's 85th row 38 and 43; and from rest at a 100-row pane's foot, 88 rows up, 10
/// standing on the prompt's row; and 2026-11-08, from rest on the wide pane, 128 along the
/// prompt's row to the 290th column (the rows over and under it), 153 five rows down there,
/// 113 forty rows down there on a pane [`TALL_ROWS`] tall, and 359 along the row from the 15th
/// column to the 990th of a pane [`WIDEST_COLS`] wide (ATERM-KI-1107-3's scenes).
const FAR_LEAPS: [FarLeap; 14] = [
    FarLeap {
        name: "40j from rest",
        grid: (TALL_ROWS, COLS),
        typed: false,
        from: (5, 58),
        to: (45, 58),
        bites: [25, 0],
    },
    FarLeap {
        name: "58j from rest",
        grid: (TALL_ROWS, COLS),
        typed: false,
        from: (5, 58),
        to: (63, 58),
        bites: [28, 0],
    },
    FarLeap {
        name: "70j from rest",
        grid: (TALL_ROWS, COLS),
        typed: false,
        from: (5, 58),
        to: (75, 58),
        bites: [8, 0],
    },
    FarLeap {
        name: "40j after the Backspaces",
        grid: (TALL_ROWS, COLS),
        typed: true,
        from: (5, 15),
        to: (45, 15),
        bites: [6, 7],
    },
    FarLeap {
        name: "58j after the Backspaces",
        grid: (TALL_ROWS, COLS),
        typed: true,
        from: (5, 15),
        to: (63, 15),
        bites: [6, 7],
    },
    FarLeap {
        name: "70j after the Backspaces",
        grid: (TALL_ROWS, COLS),
        typed: true,
        from: (5, 15),
        to: (75, 15),
        bites: [6, 7],
    },
    FarLeap {
        name: "5j to the 290th column after the Backspaces",
        grid: (ROWS, WIDE_COLS),
        typed: true,
        from: (5, 15),
        to: (10, 290),
        bites: [24, 26],
    },
    FarLeap {
        name: "40j from rest on a 160-row pane's 85th row",
        grid: (160, COLS),
        typed: false,
        from: (85, 58),
        to: (125, 58),
        bites: [25, 0],
    },
    FarLeap {
        name: "58j from rest on a 160-row pane's 85th row",
        grid: (160, COLS),
        typed: false,
        from: (85, 58),
        to: (143, 58),
        bites: [28, 0],
    },
    FarLeap {
        name: "88k from rest at a 100-row pane's foot",
        grid: (TALL_ROWS, COLS),
        typed: false,
        from: (98, 58),
        to: (10, 58),
        bites: [6, 0],
    },
    FarLeap {
        name: "along the prompt's row to the 290th column from rest",
        grid: (ROWS, WIDE_COLS),
        typed: false,
        from: (5, 15),
        to: (5, 290),
        bites: [85, 0],
    },
    FarLeap {
        name: "5j to the 290th column from rest",
        grid: (ROWS, WIDE_COLS),
        typed: false,
        from: (5, 15),
        to: (10, 290),
        bites: [102, 0],
    },
    FarLeap {
        name: "40j to the 290th column from rest",
        grid: (TALL_ROWS, WIDE_COLS),
        typed: false,
        from: (5, 15),
        to: (45, 290),
        bites: [75, 0],
    },
    FarLeap {
        name: "along the prompt's row to the 990th column from rest",
        grid: (ROWS, WIDEST_COLS),
        typed: false,
        from: (5, 15),
        to: (5, 990),
        bites: [239, 0],
    },
];

/// The frames (ms) from [`FROM`] in which `scene`'s box meets letters the head law keeps the
/// body off (cells `ch` px tall): the row's beside the prompt's that its leaps do not cross (the
/// row above's for a leap down, the row under's for one up, both for a leap along the prompt's
/// row), and for a leap up, the row above's while the box's foot stands on the prompt's row,
/// before the flight crosses it.
fn kept_off(frames: &[Frame], scene: FarLeap, ch: usize) -> Vec<u64> {
    let prompt = usize::from(scene.from.0) - 1;
    if scene.to.0 > scene.from.0 {
        return on_letters_of(frames, prompt - 1);
    }
    let along = scene.to.0 == scene.from.0;
    let ground = ((prompt + 1) * ch) as i32;
    frames
        .iter()
        .filter(|f| {
            f.ms >= FROM
                && f.boxed.iter().any(|c| {
                    c.0 == prompt + 1
                        || (c.0 == prompt - 1 && (along || f.pet.is_some_and(|p| p.3 >= ground)))
                })
        })
        .map(|f| f.ms)
        .collect()
}

/// What a [`far_leap`] page holds beside the prompt's row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    /// Rows lettered to the margin over and under it.
    Between,
    /// Nothing.
    Blank,
    /// A row lettered to the margin two over it, the row between blank: the row a bound's arc
    /// keeps its head under (`PetBrain::arc_under_letters`).
    TwoOver,
}

/// `scene` on `cell` px cells, on the `page` (a sentence written on across the pane where a row
/// is lettered), the laws in `old` reverted, keys typed as `ctl` says ([`replay`]).
fn far_leap(
    cell: (usize, usize),
    style: &str,
    (scene, page): (FarLeap, Page),
    old: &[OldPath],
    ctl: bool,
) -> Vec<Frame> {
    let row: String = "the quick brown fox jumps over the lazy dog "
        .repeat(usize::from(scene.grid.1))
        .chars()
        .take(usize::from(scene.grid.1))
        .collect();
    let prompt = scene.from.0;
    let page = match page {
        Page::Between => steam_page_at(prompt, (&row, &row)),
        Page::Blank => steam_page_at(prompt, ("", "")),
        Page::TwoOver => format!(
            "\x1b[?1049h\x1b[2J\x1b[{}H{row}\x1b[{prompt}H$ ",
            prompt - 2
        ),
    };
    let at = |(r, c): (u16, u16)| format!("\x1b[{r};{c}H").into_bytes();
    let mut ev = vec![(0, Ev::Out(page.into_bytes()))];
    let (leaps, tail) = if scene.typed {
        ev.extend(typed(TYPING_TEXT, TYPING_FROM, 60));
        let erase_from = TYPING_FROM + 60 * TYPING_TEXT.len() as u64 + 300;
        ev.extend((0..12u64).map(|k| {
            (
                erase_from + 50 * k,
                Ev::Key('\u{7f}', b"\x08 \x08".to_vec()),
            )
        }));
        let last = erase_from + 50 * 11;
        let then = [scene.to, scene.from, scene.to, scene.from];
        (
            then.iter()
                .enumerate()
                .map(|(k, &to)| (last + 300 + 1_200 * k as u64, to))
                .collect::<Vec<_>>(),
            1_600,
        )
    } else {
        let then = [scene.from, scene.to, scene.from, scene.to, scene.from];
        (
            then.iter()
                .enumerate()
                .map(|(k, &to)| (300 + 1_200 * k as u64, to))
                .collect::<Vec<_>>(),
            3_000,
        )
    };
    ev.extend(leaps.into_iter().map(|(ms, to)| (ms, Ev::Out(at(to)))));
    replay((cell, scene.grid), style, &ev, tail, old, ctl)
}

/// A BODY FAR FROM THE CARET READS ITS OWN ROWS ([`OldPath::WorldReadsTheBodysRows`]), on every
/// face's cells ([`CELLS`]): a pane past the rows or the columns the console's map reads at once
/// is read about its caret, and about the body in a second window
/// ([`crate::pet_world::PetWorld::body_window`]), so the head law reads the rows over and under
/// a body wherever the caret leaps. On the alternate screen between rows lettered to the margin
/// ([`FAR_LEAPS`]), the program moves the caret far from the body and back, twice: 40, 58 and
/// 70 rows down on a pane [`TALL_ROWS`] tall, from rest at the prompt's 58th column and after
/// the Backspaces at its 15th; five rows down to the 290th column on a pane [`WIDE_COLS`] wide,
/// after the Backspaces and from rest, and 40 rows down to it from rest on a pane that wide and
/// [`TALL_ROWS`] tall; along the prompt's row from rest, from its 15th column to its 290th on a
/// pane [`WIDE_COLS`] wide and to its 990th on one [`WIDEST_COLS`] wide, a way longer than
/// either window reads, which the head law reads as far as the window that reads the body's
/// columns reaches (ATERM-KI-1107-3); 40 and 58 rows down from rest on the 85th row of a pane 160
/// rows tall, where the window about the body reads rows far down the pane; and 88 rows up
/// from rest at the foot of a pane [`TALL_ROWS`] tall. The body gathers to leap where it rests
/// fitted to its row, far from the caret. From 600 ms, the cat and the dog, both ways of typing
/// (a scene from rest types no key, and runs once): no frame's box meets the letters the head
/// law keeps it off ([`kept_off`]: the row's beside the prompt's that the leaps do not cross,
/// both for a leap along the prompt's row, and for the leap up the row above's while the box
/// stands on the prompt's row), and none moves more than [`STEP_ROWS`] rows from the frame
/// before (measured at most 2.3). On a blank page the same leaps find no letters, and no frame
/// draws the body down its row or fits it: the rows read about the body are read as they are,
/// blank. Negative control (the law reverted alone, the cat and the dog, both ways of typing):
/// the map reads about the caret alone and a read longer than a window reads nothing, the head
/// law reads none of the body's rows and lets its fit go as it gathers to leap or sets off, and
/// its box meets those letters in each scene's [`FarLeap::bites`] frames or more (measured on
/// Linux arm64, 2026-11-07: 38 or 39, 43 and 12 from rest; 9 or 10 at the window and 11 or 12
/// as the gallery types after the Backspaces on the tall pane, 37 and 39 on the wide one; 38 or
/// 39 and 43 on the 160-row pane; 10 at the foot, standing on the prompt's row; 2026-11-08: 153
/// and 113 to the 290th column from rest, 128 along the row to it, 359 along the row to the
/// 990th). In every scene the body follows the caret away: off the prompt's rows in 60 frames
/// or more (90 to 308), and along the row, from the caret's first leap, drawn with its box's
/// middle past halfway in 34 or more (52 to 136; on the pane [`WIDEST_COLS`] wide it runs to
/// the 602nd column). With the law, between the lettered rows the head law holds it, drawn down
/// or fitted, in 100 frames or more (164 to 469); reverted, the fit lets go where the law reads
/// nothing, so the control is not held to that (63 frames on the pane [`WIDEST_COLS`] wide).
#[test]
fn a_body_far_from_the_caret_reads_its_own_rows_on_every_face() {
    let mut runs = Vec::new();
    for scene in FAR_LEAPS {
        for cell in CELLS {
            for style in STYLES {
                for ctl in if scene.typed {
                    &[false, true][..]
                } else {
                    &[false][..]
                } {
                    for (page, control) in [
                        (Page::Between, false),
                        (Page::Blank, false),
                        (Page::Between, true),
                    ] {
                        runs.push((scene, cell, style, *ctl, page, control));
                    }
                }
            }
        }
    }
    let seen = aterm_spec::check::each(&runs, |&(scene, cell, style, ctl, page, control)| {
        let old: &[OldPath] = if control {
            &[OldPath::WorldReadsTheBodysRows]
        } else {
            &[]
        };
        let frames = far_leap(cell, style, (scene, page), old, ctl);
        let held: Vec<u64> = frames
            .iter()
            .filter(|f| f.ms >= FROM && (f.sink_fit.0 > 0.0 || f.sink_fit.1 < 1.0))
            .map(|f| f.ms)
            .collect();
        (
            away(&frames, scene, cell),
            kept_off(&frames, scene, cell.1),
            held,
            largest_step(&frames, cell.1),
        )
    });
    for (&(scene, cell, style, ctl, page, control), (away, met, held, step)) in
        runs.iter().zip(&seen)
    {
        let name = scene.name;
        let lettered = page == Page::Between;
        let follows = if scene.to.0 == scene.from.0 { 34 } else { 60 };
        assert!(
            *away >= follows,
            "fixture: {style} on {cell:?} ({name}, ctl {ctl}) follows the caret ({away} frames)"
        );
        assert!(
            !lettered || control || held.len() >= 100,
            "fixture: {style} on {cell:?} ({name}, ctl {ctl}) is fitted under the lettered rows \
             ({} frames)",
            held.len()
        );
        if control {
            let bites = scene.bites[usize::from(ctl)];
            assert!(
                met.len() >= bites,
                "negative control on {cell:?} ({style}, {name}, ctl {ctl}): read about the caret \
                 alone, the box met the letters it is kept off only in the frames at {met:?} ms"
            );
        } else if lettered {
            assert!(
                met.is_empty() && *step <= STEP_ROWS,
                "{style} on {cell:?} ({name}, ctl {ctl}): frames at {met:?} ms whose box meets \
                 the letters it is kept off, a box moved {step} rows in a frame"
            );
        } else {
            assert!(
                met.is_empty() && held.is_empty() && *step <= STEP_ROWS,
                "{style} on {cell:?} ({name}, ctl {ctl}, a blank page): frames at {met:?} ms on \
                 letters, frames at {held:?} ms drawn down or fitted under blank rows, a box \
                 moved {step} rows in a frame"
            );
        }
    }
}

/// The leaps of [`a_bound_along_the_row_keeps_its_head_under_the_row_two_over_on_every_face`],
/// from rest at the prompt's 15th column on a pane [`WIDE_COLS`] wide to the 290th column some
/// rows down and back, under a row lettered two over the prompt's ([`Page::TwoOver`]). Each
/// control's floor ([`FarLeap::bites`], the first) counts the frames over all of the scene's
/// controls (the cat and the dog, 13 x 29, 14 x 26 and 11 x 28 px cells): two thirds of those
/// measured on Linux arm64, 2026-11-08.
const ARC_LEAPS: [FarLeap; 2] = [
    FarLeap {
        name: "5j to the 290th column from rest, a row two over the prompt lettered",
        grid: (ROWS, WIDE_COLS),
        typed: false,
        from: (5, 15),
        to: (10, 290),
        bites: [6, 0],
    },
    FarLeap {
        name: "40j to the 290th column from rest, a row two over the prompt lettered",
        grid: (TALL_ROWS, WIDE_COLS),
        typed: false,
        from: (5, 15),
        to: (45, 290),
        bites: [6, 0],
    },
];

/// A BOUND ALONG THE ROW PAST EITHER WINDOW KEEPS ITS HEAD UNDER THE ROW TWO OVER
/// (`PetBrain::arc_under_letters`, read about the body, [`OldPath::WorldReadsTheBodysRows`]), on
/// every face's cells ([`CELLS`]): on a pane past the columns the console's map reads at once,
/// under a row lettered two over the prompt's, the row between blank ([`ARC_LEAPS`]), the caret
/// leaps from rest to the 290th column some rows down and back, twice. The body follows it, and
/// comes back to the prompt's row in a bound along a row whose arc's way, 275 columns, is longer
/// than either window reads: the arc reads the row two over as far either way from the body as
/// the window about it reaches, finds its letters and keeps its head under them. From 600 ms,
/// the cat and the dog: no frame's box meets the letters two rows over the prompt's (the box's
/// top measured at 88, 79 and 85 px on 13 x 29, 14 x 26 and 11 x 28 px cells, under that row's
/// foot at 87, 78 and 84). Negative control (the law reverted alone): the arc reads nothing of a
/// way that long and rises its whole arc, its box meets those letters in 1 or 2 frames of
/// each run, the scene's [`FarLeap::bites`] or more over its six (measured 10 in each scene,
/// its top at 79, 74 and 76 px, Linux arm64, 2026-11-08). In every run the body follows the
/// caret off the prompt's rows in 60 frames or more (179 to 255).
#[test]
fn a_bound_along_the_row_keeps_its_head_under_the_row_two_over_on_every_face() {
    let mut runs = Vec::new();
    for scene in ARC_LEAPS {
        for cell in CELLS {
            for style in STYLES {
                for control in [false, true] {
                    runs.push((scene, cell, style, control));
                }
            }
        }
    }
    let seen = aterm_spec::check::each(&runs, |&(scene, cell, style, control)| {
        let old: &[OldPath] = if control {
            &[OldPath::WorldReadsTheBodysRows]
        } else {
            &[]
        };
        let frames = far_leap(cell, style, (scene, Page::TwoOver), old, false);
        let prompt = usize::from(scene.from.0) - 1;
        (
            away(&frames, scene, cell),
            on_letters_of(&frames, prompt - 2),
        )
    });
    for scene in ARC_LEAPS {
        let met: usize = runs
            .iter()
            .zip(&seen)
            .filter(|(run, _)| run.0.name == scene.name && run.3)
            .map(|(_, (_, met))| met.len())
            .sum();
        assert!(
            met >= scene.bites[0],
            "negative control ({}): read about the caret alone, the bound's box met the letters \
             two rows over the prompt's in only {met} frames",
            scene.name
        );
    }
    for (&(scene, cell, style, control), (away, met)) in runs.iter().zip(&seen) {
        let name = scene.name;
        assert!(
            *away >= 60,
            "fixture: {style} on {cell:?} ({name}) follows the caret ({away} frames)"
        );
        assert!(
            control || met.is_empty(),
            "{style} on {cell:?} ({name}): frames at {met:?} ms whose box meets the letters two \
             rows over the prompt's"
        );
    }
}

/// How many rows a frame's box's top may move from the frame before's, at the most
/// ([`a_body_far_from_the_caret_reads_its_own_rows_on_every_face`],
/// [`a_flight_re_aimed_keeps_its_take_off_top_while_its_way_is_down_on_every_face`]): a body's
/// flight carries its box no further in a frame (measured at most 2.3 rows).
const STEP_ROWS: f32 = 3.0;

/// The most rows a frame's box's top moved from the frame before's, from [`FROM`] (cells `ch`
/// px tall).
fn largest_step(frames: &[Frame], ch: usize) -> f32 {
    frames
        .windows(2)
        .filter(|w| w[1].ms >= FROM)
        .filter_map(|w| Some((w[1].pet?.2 - w[0].pet?.2).abs()))
        .max()
        .map_or(0.0, |px| px as f32 / ch as f32)
}

/// How many frames show the body followed `scene`'s caret away, on `cell` px cells: from
/// [`FROM`], off the prompt's rows ([`away_from_the_prompt`]); for a leap along the prompt's row,
/// from the caret's first leap ([`LEFT_REST`]), drawn with the box's middle past the column
/// halfway from the rest to the leap's. A frame that draws no body counts in neither: a body
/// faded out has followed nothing, and meets no letters.
fn away(frames: &[Frame], scene: FarLeap, (cw, ch): (usize, usize)) -> usize {
    if scene.to.0 != scene.from.0 {
        return away_from_the_prompt(frames, usize::from(scene.from.0) - 1, ch);
    }
    let half = (i32::from(scene.from.1) + i32::from(scene.to.1) - 2) * cw as i32 / 2;
    let right = scene.to.1 > scene.from.1;
    frames
        .iter()
        .filter(|f| {
            f.ms >= LEFT_REST
                && f.pet.is_some_and(|p| {
                    let mid = (p.0 + p.1) / 2;
                    if right { mid > half } else { mid < half }
                })
        })
        .count()
}

/// When (ms) a [`far_leap`] from rest has the caret leave the rest for the first time.
const LEFT_REST: u64 = 1_500;

/// How many frames from [`FROM`] have the box's foot past the row under the prompt's row
/// `prompt` (counted from 0), or over the row above it (cells `ch` px tall): the body has
/// followed the caret away.
fn away_from_the_prompt(frames: &[Frame], prompt: usize, ch: usize) -> usize {
    let (over, under) = (((prompt - 1) * ch) as i32, ((prompt + 2) * ch) as i32);
    frames
        .iter()
        .filter(|f| f.ms >= FROM && f.pet.is_some_and(|p| p.3 <= over || p.3 > under))
        .count()
}

/// The leaps up from a pane's foot and back
/// ([`a_flight_re_aimed_keeps_its_take_off_top_while_its_way_is_down_on_every_face`]), from rest at the
/// prompt's 58th column on the pane's last row but two: 30 rows up a pane 40 rows tall, 52 up
/// one 64 rows tall, and 88 up one [`TALL_ROWS`] tall. Each control's floor is two thirds of the
/// fewest frames it was measured at on Linux arm64, 2026-11-07 (cat and dog, 13 x 29, 14 x 26
/// and 11 x 28 px cells): 63, 73 and 179.
const TURNED_LEAPS: [FarLeap; 3] = [
    FarLeap {
        name: "30k from rest at a 40-row pane's foot",
        grid: (40, COLS),
        typed: false,
        from: (38, 58),
        to: (8, 58),
        bites: [42, 0],
    },
    FarLeap {
        name: "52k from rest at a 64-row pane's foot",
        grid: (64, COLS),
        typed: false,
        from: (62, 58),
        to: (10, 58),
        bites: [48, 0],
    },
    FarLeap {
        name: "88k from rest at a 100-row pane's foot",
        grid: (TALL_ROWS, COLS),
        typed: false,
        from: (98, 58),
        to: (10, 58),
        bites: [119, 0],
    },
];

/// A crossing down off a held row re-aimed further down in the air
/// ([`a_flight_re_aimed_keeps_its_take_off_top_while_its_way_is_down_on_every_face`]), on `cell`
/// px cells, the laws in `old` reverted: on the `steam` scene's pane between rows lettered to the
/// margin, from rest at the prompt's 58th column, the program moves the caret three rows down
/// and five more 300 ms later, as the body is in the air, and back 1.2 s after the first; twice,
/// 2.4 s apart.
fn crossing_re_aimed_down(cell: (usize, usize), style: &str, old: &[OldPath]) -> Vec<Frame> {
    let row: String = "the quick brown fox jumps over the lazy dog "
        .repeat(usize::from(COLS))
        .chars()
        .take(usize::from(COLS))
        .collect();
    let at = |r: u16| Ev::Out(format!("\x1b[{r};58H").into_bytes());
    let mut ev = vec![
        (0, Ev::Out(steam_page((&row, &row)).into_bytes())),
        (300, at(5)),
    ];
    for k in 0..2 {
        let t = 1_500 + 2_400 * k;
        ev.extend([(t, at(8)), (t + 300, at(13)), (t + 1_200, at(5))]);
    }
    replay((cell, (ROWS, COLS)), style, &ev, 3_000, old, false)
}

/// A FLIGHT RE-AIMED KEEPS ITS TAKE-OFF TOP WHILE ITS WAY IS DOWN
/// ([`OldPath::TurnKeepsTheTakeOffTop`]), on every face's cells ([`CELLS`]): a body the head law
/// holds fitted between rows lettered to the margin takes off with its box's top kept, which
/// keeps a crossing down off its row under it (`PetBrain::left_top`), and a flight re-aimed in
/// the air whose way goes up drops it (`PetBrain::rebase`). On the alternate screen at a pane's
/// foot ([`TURNED_LEAPS`]) the program moves the caret far up and back while the body is in the
/// air, twice, and the flight up is turned back down. From 600 ms, the cat and the dog: no
/// frame's box meets the row under's letters, and none moves more than [`STEP_ROWS`] rows from
/// the frame before (measured at most 0.8, 1.5 and 1.9 rows). Negative control (the law reverted
/// alone, the cat and the dog): the box, held at its take-off top as the body comes down from
/// high over it, moves 26, 29 or 47 rows in one frame and lies on the row under's letters in
/// each scene's [`FarLeap::bites`] frames or more (measured 63, 73 and 179). A crossing down
/// re-aimed further down keeps its top ([`crossing_re_aimed_down`]): no frame's box meets the
/// row above's letters, and none moves more than [`STEP_ROWS`] rows; its control, the crossing
/// drawn whole ([`OldPath::CrossingRisesIntoTheLine`] reverted, the cat and the dog), meets them
/// in ten frames or more (measured 15 or 16). In every scene the body follows the caret away
/// from the prompt's rows in 60 frames or more.
#[test]
fn a_flight_re_aimed_keeps_its_take_off_top_while_its_way_is_down_on_every_face() {
    let mut runs = Vec::new();
    for cell in CELLS {
        for style in STYLES {
            for control in [false, true] {
                for scene in TURNED_LEAPS {
                    runs.push((Some(scene), cell, style, control));
                }
                runs.push((None, cell, style, control));
            }
        }
    }
    let seen = aterm_spec::check::each(&runs, |&(scene, cell, style, control)| {
        let (frames, prompt) = match scene {
            Some(scene) => {
                let old: &[OldPath] = if control {
                    &[OldPath::TurnKeepsTheTakeOffTop]
                } else {
                    &[]
                };
                let frames = far_leap(cell, style, (scene, Page::Between), old, false);
                (frames, usize::from(scene.from.0) - 1)
            }
            None => {
                let old: &[OldPath] = if control {
                    &[OldPath::CrossingRisesIntoTheLine]
                } else {
                    &[]
                };
                (crossing_re_aimed_down(cell, style, old), PROMPT_ROW)
            }
        };
        (
            away_from_the_prompt(&frames, prompt, cell.1),
            on_letters_of(&frames, prompt - 1),
            on_letters_of(&frames, prompt + 1),
            largest_step(&frames, cell.1),
        )
    });
    for (&(scene, cell, style, control), (away, above, under, step)) in runs.iter().zip(&seen) {
        let name = scene.map_or("a crossing re-aimed down", |s| s.name);
        assert!(
            *away >= 60,
            "fixture: {style} on {cell:?} ({name}) follows the caret ({away} frames)"
        );
        match (scene, control) {
            (Some(scene), true) => assert!(
                under.len() >= scene.bites[0] && *step > STEP_ROWS,
                "negative control on {cell:?} ({style}, {name}): with its take-off top kept, the \
                 box met the row under's letters only in the frames at {under:?} ms, and moved \
                 {step} rows in a frame at the most"
            ),
            (Some(_), false) => assert!(
                under.is_empty() && *step <= STEP_ROWS,
                "{style} on {cell:?} ({name}): frames at {under:?} ms whose box meets the row \
                 under's letters, a box moved {step} rows in a frame"
            ),
            (None, true) => assert!(
                above.len() >= 10,
                "negative control on {cell:?} ({style}, {name}): drawn whole, the crossing met \
                 the row above's letters only in the frames at {above:?} ms"
            ),
            (None, false) => assert!(
                above.is_empty() && *step <= STEP_ROWS,
                "{style} on {cell:?} ({name}): frames at {above:?} ms whose box meets the row \
                 above's letters, a box moved {step} rows in a frame"
            ),
        }
    }
}

/// The leaps of [`a_body_outside_the_caret_s_window_stays_on_glass_on_every_face`]: from rest,
/// along the prompt's row of a pane past the columns the console's map reads at once, from a
/// column the caret's window leaves once the caret leaps to ten columns short of the pane's end.
/// Each control's floor ([`FarLeap::bites`], the first) counts the frames off the glass: two
/// thirds of the fewest measured on Linux arm64, 2026-11-08 (cat and dog, 13 x 29, 14 x 26 and
/// 11 x 28 px cells, between lettered rows and on a blank page alike): 84, 89, 103, 146, 312, 347
/// and 53.
const FAR_ALONG: [FarLeap; 7] = [
    FarLeap {
        name: "along the prompt's row from the 20th column to the 290th",
        grid: (ROWS, WIDE_COLS),
        typed: false,
        from: (5, 20),
        to: (5, 290),
        bites: [56, 0],
    },
    FarLeap {
        name: "along the prompt's row from the 30th column to the 290th",
        grid: (ROWS, WIDE_COLS),
        typed: false,
        from: (5, 30),
        to: (5, 290),
        bites: [59, 0],
    },
    FarLeap {
        name: "along the prompt's row from the 30th column to the 340th",
        grid: (ROWS, 350),
        typed: false,
        from: (5, 30),
        to: (5, 340),
        bites: [68, 0],
    },
    FarLeap {
        name: "along the prompt's row from the 30th column to the 503rd",
        grid: (ROWS, 513),
        typed: false,
        from: (5, 30),
        to: (5, 503),
        bites: [97, 0],
    },
    FarLeap {
        name: "along the prompt's row from the 30th column to the 590th",
        grid: (ROWS, 600),
        typed: false,
        from: (5, 30),
        to: (5, 590),
        bites: [208, 0],
    },
    FarLeap {
        name: "along the prompt's row from the 30th column to the 990th",
        grid: (ROWS, WIDEST_COLS),
        typed: false,
        from: (5, 30),
        to: (5, 990),
        bites: [231, 0],
    },
    FarLeap {
        name: "along the prompt's row from the 15th column to the 990th",
        grid: (ROWS, WIDEST_COLS),
        typed: false,
        from: (5, 15),
        to: (5, 990),
        bites: [35, 0],
    },
];

/// A BODY OUTSIDE THE CARET'S WINDOW STANDS ON THE GROUND THE WINDOW ABOUT IT READS
/// ([`OldPath::BodysWindowReadsItsGround`]), on every face's cells ([`CELLS`]): the console's
/// visibility hold judges the ground under the body's box (`PetBrain::ground_clearance`), and
/// where that box lies outside the window
/// the map reads about the caret, the window about the body
/// ([`crate::pet_world::PetWorld::body_window`]) answers it, its cells' ground read with its
/// glyphs. On a pane past the columns the map reads at once, the caret leaps from rest along the
/// prompt's row far from the body and back, twice ([`FAR_ALONG`]): from the 20th or the 30th
/// column to the 290th on a pane [`WIDE_COLS`] wide, from the 30th to ten columns short of the
/// end of panes 350, 513, 600 and [`WIDEST_COLS`] wide, and from the 15th to the 990th on the
/// widest, where the body runs to the 602nd column and rests there while the caret is back. From
/// 600 ms, the cat and the dog, between rows lettered to the margin and on a blank page: every
/// frame draws the body at full alpha, and it follows the caret away, from the caret's first
/// leap drawn with its box's middle past halfway in 60 frames or more (measured 89 to 187).
/// Negative control (the law reverted alone, the cat and the dog, both pages): the window about
/// the body reads glyphs alone, the hold reads nothing of a box outside the caret's window, goes
/// blind, and takes the body off the glass, and a hidden body's flight keeps it off until the
/// flight lands. It is off the glass in each scene's [`FarLeap::bites`] frames or more (measured
/// 84 and 89 on the wide pane, 103, 146, 312 and 347 on the wider ones from the 30th column, the
/// fade starting 2.32 or 2.37 s in, 0.82 to 0.87 s past the caret's leap; 53 from the 15th on the
/// widest, its fade starting at 7.07 s with the body at rest at the 602nd column, Linux arm64,
/// 2026-11-08), and on the panes 600 and [`WIDEST_COLS`] wide it follows the caret in none
/// (ATERM-KI-1107-3).
#[test]
fn a_body_outside_the_caret_s_window_stays_on_glass_on_every_face() {
    let mut runs = Vec::new();
    for scene in FAR_ALONG {
        for cell in CELLS {
            for style in STYLES {
                for page in [Page::Between, Page::Blank] {
                    for control in [false, true] {
                        runs.push((scene, cell, style, page, control));
                    }
                }
            }
        }
    }
    let seen = aterm_spec::check::each(&runs, |&(scene, cell, style, page, control)| {
        let old: &[OldPath] = if control {
            &[OldPath::BodysWindowReadsItsGround]
        } else {
            &[]
        };
        let frames = far_leap(cell, style, (scene, page), old, false);
        let judged = || frames.iter().filter(|f| f.ms >= FROM);
        let faded: Vec<u64> = judged()
            .filter(|f| f.alpha < u8::MAX)
            .map(|f| f.ms)
            .collect();
        let off = judged().filter(|f| f.alpha == 0).count();
        (away(&frames, scene, cell), faded, off)
    });
    for (&(scene, cell, style, page, control), (away, faded, off)) in runs.iter().zip(&seen) {
        let why = format!("{style} on {cell:?} ({}, {page:?})", scene.name);
        if control {
            assert!(
                *off >= scene.bites[0],
                "negative control, {why}: its window read glyphs alone, the body was off the \
                 glass in only {off} frames"
            );
            continue;
        }
        assert!(
            faded.is_empty() && *away >= 60,
            "{why}: frames below full alpha at {faded:?} ms, or it followed the caret away in \
             only {away} frames"
        );
    }
}

/// The row over the prompt of [`a_gap_over_the_body_is_read_about_it_on_every_face`]: three
/// words 24 cells apart (`abcd`, `efgh`, `ijkl`, the last ending at the 60th column), each gap
/// wider than the body on any face.
const GAPPED_ROW_END: u16 = 60;

/// [`a_gap_over_the_body_is_read_about_it_on_every_face`]'s replay on `cell` px cells, a pane
/// [`ROWS`] rows by `cols` columns: the gapped row ([`GAPPED_ROW_END`]) over the prompt's,
/// [`TYPING_TEXT`] typed at the window from [`TYPING_FROM`], a key every 60 ms, the laws in `old`
/// reverted.
fn under_a_gapped_row(cell: (usize, usize), style: &str, cols: u16, old: &[OldPath]) -> Vec<Frame> {
    let row = format!("abcd{0}efgh{0}ijkl", " ".repeat(24));
    debug_assert_eq!(row.len(), usize::from(GAPPED_ROW_END));
    // Rows counted from 1: the words on the row over the prompt's.
    let page = format!("\x1b[2J\x1b[{}H{row}\x1b[{}H$ ", PROMPT_ROW, PROMPT_ROW + 1);
    let mut ev = vec![(0, Ev::Out(page.into_bytes()))];
    ev.extend(typed(TYPING_TEXT, TYPING_FROM, 60));
    replay((cell, (ROWS, cols)), style, &ev, 1_600, old, false)
}

/// The frames from [`FROM`] standing on the row `row` (counted from 0, cells `ch` px tall), once
/// the head law first holds the box in it (its top at that row's top or under it), whose box's
/// top rises more than a quarter of a row past that row's top again with its left edge short of
/// `line_end` px: the body rose whole in a gap of the row above, under its words.
fn rose_in_a_gap(frames: &[Frame], row: usize, ch: usize, line_end: i32) -> usize {
    let ch = ch as i32;
    let top = row as i32 * ch;
    let feet = (top + ch / 2)..((row as i32 + 2) * ch);
    let boxes: Vec<(i32, i32)> = frames
        .iter()
        .filter(|f| f.ms >= FROM)
        .filter_map(|f| f.pet)
        .filter(|p| feet.contains(&p.3))
        .map(|p| (p.0, p.2))
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

/// A GAP OVER THE BODY IS READ ABOUT IT ([`OldPath::GapReadAboutTheBody`]), on every face's cells
/// ([`CELLS`]): the head law holds a body in its row across a gap of the row above, a word on
/// either side of it (`PetBrain::gap_above`, [`OldPath::HeadHeldAcrossGaps`]), and reads each side
/// from the body's edge as far as the window that reads the cell beside it reaches, so a side
/// longer than one window reads ([`crate::pet_world::MAX_WORLD_COLS`]) is read that far, not
/// refused. Three words 24 cells apart on the row over the prompt ([`under_a_gapped_row`]), the
/// line typed under them, on panes 72, 256, 257, 300 and 1,000 columns wide. From 600 ms, the cat
/// and the dog: once the law holds the body in the prompt's row, no frame's box rises past it
/// under the words ([`rose_in_a_gap`]), and the box meets no letter of the row above; the law holds
/// it, drawn down or fitted, in 150 frames or more (measured 211 on every pane). Negative control
/// (the law reverted alone, the cat and the dog, the panes past 256 columns): a side read past
/// one window reads nothing and counts as no ink, no gap, and the body rises whole in each gap,
/// in 23 frames or more, two thirds of the fewest measured (35 on 11 x 28, 38 on 13 x 29, 43 on
/// 14 x 26 on each of those panes, as with [`OldPath::HeadHeldAcrossGaps`] reverted on every
/// pane; held in 175 to 183; on the panes up to 256 columns, where a side is never longer than a
/// window reads, the same as with the law, Linux arm64, 2026-11-08; ATERM-KI-1107-3).
#[test]
fn a_gap_over_the_body_is_read_about_it_on_every_face() {
    const PANES: [u16; 5] = [72, 256, 257, 300, 1_000];
    let mut runs = Vec::new();
    for cols in PANES {
        for cell in CELLS {
            for style in STYLES {
                runs.push((cols, cell, style, false));
                if usize::from(cols) > crate::pet_world::MAX_WORLD_COLS {
                    runs.push((cols, cell, style, true));
                }
            }
        }
    }
    let prompt = PROMPT_ROW;
    let seen = aterm_spec::check::each(&runs, |&(cols, cell, style, control)| {
        let old: &[OldPath] = if control {
            &[OldPath::GapReadAboutTheBody]
        } else {
            &[]
        };
        let frames = under_a_gapped_row(cell, style, cols, old);
        let held = frames
            .iter()
            .filter(|f| f.ms >= FROM && (f.sink_fit.0 > 0.0 || f.sink_fit.1 < 1.0))
            .count();
        let line_end = i32::from(GAPPED_ROW_END) * cell.0 as i32;
        (
            rose_in_a_gap(&frames, prompt, cell.1, line_end),
            on_letters_of(&frames, prompt - 1),
            held,
        )
    });
    for (&(cols, cell, style, control), (rose, met, held)) in runs.iter().zip(&seen) {
        let why = format!("{style} on {cell:?}, a pane {cols} columns wide");
        if control {
            assert!(
                *rose >= 23,
                "negative control, {why}: a side past one window refused, it rose in a gap in \
                 only {rose} frames"
            );
            continue;
        }
        assert!(
            *rose == 0 && met.is_empty() && *held >= 150,
            "{why}: it rose in a gap in {rose} frames, met the words at {met:?} ms, or was held \
             in {held} frames"
        );
    }
}

/// The run of spaces before the last letters of [`walked_through_a_gap`]'s line.
const WALKED_GAP: usize = 5;
/// The length (cells, its prompt's with it) of [`walked_through_a_gap`]'s line as the trail
/// gallery types it: [`BURST_TEXT`]'s.
const GAP_LINE: usize = "$ ".len() + BURST_TEXT.len();

/// [`a_body_kept_off_the_line_clears_the_caret_on_every_face`]'s replay on `cell` px cells, the
/// laws in `old` reverted, typed at the window or (`ctl`) as the gallery types: on
/// [`last_row_page`]'s last row, a grid four blank cells wider than the line, a line `line` cells
/// long with its prompt that ends in [`WALKED_GAP`] spaces and the first `tail` letters of
/// `abcdefghijkl` (`echo`, a run of `q`, the gap, the letters), typed from [`TYPING_FROM`] a key
/// every 50 ms. From 1.5 s past the last key, Left arrows 150 ms apart (each echoed back a cell)
/// walk the caret back across the letters and the gap to the second `q` before it; 600 ms on, as
/// many Right arrows (each echoed forward a cell) walk it back to the line's end, and 1.5 s on
/// an Enter whose answer scrolls the page and prints a new prompt. Its frames, the line's end
/// (cells), and the first arrow's and the Enter's ms.
fn walked_through_a_gap(
    (cell, line): ((usize, usize), usize),
    tail: usize,
    style: &str,
    old: &[OldPath],
    ctl: bool,
) -> (Vec<Frame>, usize, (u64, u64)) {
    let lead = line - "$ ".len() - "echo ".len() - WALKED_GAP - tail;
    let text = format!(
        "echo {}{}{}",
        "q".repeat(lead),
        " ".repeat(WALKED_GAP),
        &"abcdefghijkl"[..tail]
    );
    let cols = u16::try_from(line + 4).expect("a grid as wide as the line");
    let mut ev = vec![(200, Ev::Out(last_row_page(cols, 0).into_bytes()))];
    ev.extend(typed(&text, TYPING_FROM, 50));
    let last_key = TYPING_FROM + 50 * (text.len() as u64 - 1);
    let keys = (tail + WALKED_GAP + 2) as u64;
    let from = last_key + 1_500;
    ev.extend((0..keys).map(|k| (from + 150 * k, Ev::Key(LEFT, b"\x08".to_vec()))));
    let back = from + 150 * keys + 600;
    ev.extend((0..keys).map(|k| (back + 150 * k, Ev::Key(RIGHT, b"\x1b[C".to_vec()))));
    let enter = back + 150 * keys + 1_500;
    ev.push((enter, Ev::Key('\r', b"\r\n".to_vec())));
    ev.push((enter + 6, Ev::Out(b"Below: pqgyj [ok]\r\n$ ".to_vec())));
    ev.sort_by_key(|e| e.0);
    let frames = replay((cell, (ROWS, cols)), style, &ev, 1_600, old, ctl);
    (frames, line, (from, enter))
}

/// How many of `frames` from `from` to the Enter (ms) draw the box over the caret's cell at full
/// alpha, the caret short of the line's end `line` (cells `cw x ch` px), and how many on glass
/// draw it on a lettered cell, each by more than `px` px across.
fn over_the_caret_and_letters(
    frames: &[Frame],
    line: usize,
    (from, enter): (u64, u64),
    (cw, ch): (usize, usize),
    px: i32,
) -> (usize, usize) {
    let (cw_px, ch_px) = (cw as i32, ch as i32);
    let held = frames.iter().filter(|f| (from..enter).contains(&f.ms));
    let over = |f: &Frame| {
        let (r, c) = (f.caret.0 as i32, f.caret.1 as i32);
        f.pet.is_some_and(|(x0, x1, y0, y1)| {
            x1.min((c + 1) * cw_px) - x0.max(c * cw_px) > px
                && y0 < (r + 1) * ch_px
                && y1 > r * ch_px
        })
    };
    let caret = held
        .clone()
        .filter(|f| f.alpha == u8::MAX && f.caret.1 < line && over(f))
        .count();
    let letters = held.filter(|f| reach_of(f, cw) > px).count();
    (caret, letters)
}

/// A BODY KEPT OFF THE LINE CLEARS THE CARET ([`OldPath::KeptOffTheLine`]), on every face's cells
/// ([`CELLS`]): the law keeps a body where it stands only off the line and clear of the caret
/// (`PetBrain::kept_off_the_line`), as every stand `PetBrain::ink_safe_col_for` offers clears it,
/// so it never trades the line's letters for the caret's cell. On a full page's last row the
/// caret walks back through a gap of the line and out again ([`walked_through_a_gap`], the line
/// ending in 2, 4, 8 and 12 letters). From the first arrow to the Enter, the cat and the dog,
/// typed at the window and as the gallery types: no run with the law draws the box over the
/// caret's cell at full alpha, the caret short of the line's end, or on a lettered cell, in more
/// frames than the same run with the law reverted, and on each face, over its runs, the law
/// does both in fewer. Measured (Linux arm64, 2026-11-08), over the caret and on letters, with
/// the law: 0 and 0 frames on 13 x 29 and 14 x 26 ([`OldPath::GapIsTheLines`] keeps it at the
/// wall there), 4 and 7 to 13 on 11 x 28 (the pose's swell at the wall, 1 px); reverted: 30 to 57
/// and 174 to 348, 5 to 18 and 132 to 243, 19 to 63 and 218 to 373. A body kept wherever its box
/// lay on no ink, the gap and the caret's cell included, stood over the caret on 14 x 26 in 28 to
/// 75 frames (on letters in 35 to 60).
#[test]
fn a_body_kept_off_the_line_clears_the_caret_on_every_face() {
    const TAILS: [usize; 4] = [2, 4, 8, 12];
    let mut runs = Vec::new();
    for cell in CELLS {
        for style in STYLES {
            for ctl in [false, true] {
                for tail in TAILS {
                    for control in [false, true] {
                        runs.push((cell, style, ctl, tail, control));
                    }
                }
            }
        }
    }
    let seen = aterm_spec::check::each(&runs, |&(cell, style, ctl, tail, control)| {
        let old: &[OldPath] = if control {
            &[OldPath::KeptOffTheLine]
        } else {
            &[]
        };
        let (frames, line, held) = walked_through_a_gap((cell, GAP_LINE), tail, style, old, ctl);
        over_the_caret_and_letters(&frames, line, held, cell, 0)
    });
    let mut faces = std::collections::BTreeMap::new();
    for (pair, seen) in runs.chunks(2).zip(seen.chunks(2)) {
        let (cell, style, ctl, tail, _) = pair[0];
        let (law, reverted) = (seen[0], seen[1]);
        assert!(
            law.0 <= reverted.0 && law.1 <= reverted.1,
            "{style} on {cell:?} (ctl {ctl}), the line ending in {tail} letters: with the law \
             over the caret in {} frames and on letters in {}, reverted in {} and {}",
            law.0,
            law.1,
            reverted.0,
            reverted.1
        );
        let face: &mut [usize; 4] = faces.entry(cell).or_default();
        face[0] += law.0;
        face[1] += law.1;
        face[2] += reverted.0;
        face[3] += reverted.1;
    }
    for (cell, [caret, letters, caret_reverted, letters_reverted]) in faces {
        assert!(
            caret < caret_reverted && letters < letters_reverted,
            "negative control on {cell:?}: with the law over the caret in {caret} frames and on \
             letters in {letters}, reverted in only {caret_reverted} and {letters_reverted}"
        );
    }
}

/// The lengths (cells, the prompt's with them) of [`walked_through_a_gap`]'s line that
/// [`a_gap_in_the_line_is_the_line_s_on_every_face`] walks: the trail gallery's, one whose grid
/// (104 columns) ends a box flush with the wall a rounding past its last column on 13 and 14 px
/// cells read by the width's reciprocal ([`PetBrain::rest_box_cols`]), and one whose grid (300
/// columns) runs past the columns the console's map reads at once
/// ([`crate::pet_world::MAX_WORLD_COLS`]).
const GAP_LINES: [usize; 3] = [GAP_LINE, 100, 296];

/// A GAP IN THE LINE IS THE LINE'S ([`OldPath::GapIsTheLines`]), on every face's cells
/// ([`CELLS`]) and at each of [`GAP_LINES`]: in
/// [`a_body_kept_off_the_line_clears_the_caret_on_every_face`]'s scene (the caret walked back
/// through a gap of the line and out again, the line ending in 2, 4, 8 and 12 letters after it,
/// four blank cells at the wall) a body at the wall keeps there while the stand by the caret lies
/// in the gap, as it keeps there while that stand lies on the letters. From the first arrow to
/// the Enter, the cat and the dog, typed at the window and as the gallery types: no frame's box
/// reaches more than 2 px into a lettered cell, nor covers more than 2 px of the caret's cell at
/// full alpha, the caret short of the line's end (the pose's swell at the wall on 11 x 28 reaches
/// 1 px into the line's last letter, the caret's cell at the first arrow).
///
/// Negative control (the law reverted alone, the cat, both ways of typing, each length): on
/// 14 x 26, where the body fits in the gap clear of the caret, it leaves the wall across the
/// line's last letters and the caret's cell for the gap, follows the caret's stand from there onto
/// the letters before it, and back onto the last letters as the caret comes out: its box lies
/// more than 2 px into them in 378 frames or more over the tails, two thirds of the fewest
/// measured (567, typed at the window on 300 columns: 35, 141, 189 and 202 at 2, 4, 8 and 12
/// letters; 570 and 577 on 104 and 73 columns; 634 to 640 as the gallery types; over the
/// caret's cell in up to 16 a run; Linux arm64, 2026-11-08). On 13 x 29 and 11 x 28 no stand in
/// the gap clears the caret, and reverted it stays at the wall as with the law.
///
/// Read off the row's span from the line's first column, the law read nothing on 300 columns,
/// past the map's; at 104 columns, its box at the wall read past the grid, it and the seat kept
/// off the line both read nothing there: with them on, 14 x 26 had the box more than 2 px into the
/// letters in 128, 105, 189 and 201 frames typed at the window on 104 columns, and 35, 141, 189
/// and 202 on 300 (Linux arm64, 2026-11-08).
#[test]
fn a_gap_in_the_line_is_the_line_s_on_every_face() {
    const TAILS: [usize; 4] = [2, 4, 8, 12];
    let mut runs = Vec::new();
    for cell in CELLS {
        for line in GAP_LINES {
            for ctl in [false, true] {
                for tail in TAILS {
                    for style in STYLES {
                        runs.push(((cell, line), style, ctl, tail, false));
                    }
                    if cell == (14, 26) {
                        runs.push(((cell, line), STYLES[0], ctl, tail, true));
                    }
                }
            }
        }
    }
    let seen = aterm_spec::check::each(&runs, |&(at, style, ctl, tail, control)| {
        let old: &[OldPath] = if control {
            &[OldPath::GapIsTheLines]
        } else {
            &[]
        };
        let (frames, line, held) = walked_through_a_gap(at, tail, style, old, ctl);
        over_the_caret_and_letters(&frames, line, held, at.0, 2)
    });
    let mut bitten = std::collections::BTreeMap::new();
    for (&((cell, line), style, ctl, tail, control), &(caret, letters)) in runs.iter().zip(&seen) {
        if control {
            *bitten.entry((line, ctl)).or_insert(0) += letters;
            continue;
        }
        assert!(
            caret == 0 && letters == 0,
            "{style} on {cell:?} (ctl {ctl}), a line {line} cells long ending in {tail} letters: \
             past 2 px over the caret in {caret} frames and into the letters in {letters}"
        );
    }
    for ((line, ctl), letters) in bitten {
        assert!(
            letters >= 378,
            "negative control on (14, 26), a line {line} cells long (ctl {ctl}, {:?}): the gap's \
             stand laid its box into the letters in only {letters} frames",
            OldPath::GapIsTheLines
        );
    }
}
