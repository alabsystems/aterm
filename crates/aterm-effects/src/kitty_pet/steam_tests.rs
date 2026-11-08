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

/// A drawn frame: when, the companion's box (`x0, x1, y0, y1` px), the lettered cells
/// (`row, col`) the box meets by a pixel or more, and how far (rows) the head law draws the body
/// down its row and the share of its height it fits it to
/// ([`EffectsPipeline::companion_head_sink_fit`]).
#[derive(Clone, Debug)]
struct Frame {
    ms: u64,
    pet: Option<(i32, i32, i32, i32)>,
    boxed: Vec<(usize, usize)>,
    sink_fit: (f32, f32),
}

/// `events` replayed on `cell` px cells, the pane `grid` rows by columns, a frame every
/// [`FRAME_MS`] to `tail` ms past the last, the laws in `old` reverted. A key's echo lands 2 ms
/// after it; typed at the window (`ctl` false) the console sees each key, as the gallery types it
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
                    sink_fit: fx.companion_head_sink_fit(),
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

/// How many rows tall the pane of [`FAR_LEAPS`]' tall scenes is: past the rows the console's map
/// reads at once ([`crate::pet_world::MAX_WORLD_ROWS`]).
const TALL_ROWS: u16 = 100;

/// How many columns wide the pane of [`FAR_LEAPS`]' wide scene is: past the columns the
/// console's map reads at once ([`crate::pet_world::MAX_WORLD_COLS`]).
const WIDE_COLS: u16 = 300;

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
    /// scene from rest: the first).
    bites: [usize; 2],
}

/// The leaps far from the body ([`FarLeap`]), each control's floor two thirds of the fewest
/// frames it was measured at on Linux arm64, 2026-11-07 (cat and dog, 13 x 29, 14 x 26 and
/// 11 x 28 px cells): from rest 38, 43 and 12 frames; after the Backspaces 9 at the window
/// and 11 as the gallery types, on the tall pane, 37 and 39 on the wide one; from rest on a
/// 160-row pane's 85th row 38 and 43; and from rest at a 100-row pane's foot, 88 rows up, 10
/// standing on the prompt's row.
const FAR_LEAPS: [FarLeap; 10] = [
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
];

/// The frames (ms) from [`FROM`] in which `scene`'s box meets letters the head law keeps the
/// body off (cells `ch` px tall): the row's beside the prompt's that its leaps do not cross (the
/// row above's for a leap down, the row under's for one up), and for a leap up, the row above's
/// while the box's foot stands on the prompt's row, before the flight crosses it.
fn kept_off(frames: &[Frame], scene: FarLeap, ch: usize) -> Vec<u64> {
    let prompt = usize::from(scene.from.0) - 1;
    if scene.to.0 > scene.from.0 {
        return on_letters_of(frames, prompt - 1);
    }
    let ground = ((prompt + 1) * ch) as i32;
    frames
        .iter()
        .filter(|f| {
            f.ms >= FROM
                && f.boxed.iter().any(|c| {
                    c.0 == prompt + 1 || (c.0 == prompt - 1 && f.pet.is_some_and(|p| p.3 >= ground))
                })
        })
        .map(|f| f.ms)
        .collect()
}

/// `scene` on `cell` px cells, between rows lettered to the margin (a sentence written on across
/// the pane, over and under the prompt's row) or on a blank page, the laws in `old` reverted,
/// keys typed as `ctl` says ([`replay`]).
fn far_leap(
    cell: (usize, usize),
    style: &str,
    (scene, lettered): (FarLeap, bool),
    old: &[OldPath],
    ctl: bool,
) -> Vec<Frame> {
    let row = if lettered {
        "the quick brown fox jumps over the lazy dog "
            .repeat(usize::from(scene.grid.1))
            .chars()
            .take(usize::from(scene.grid.1))
            .collect()
    } else {
        String::new()
    };
    let at = |(r, c): (u16, u16)| format!("\x1b[{r};{c}H").into_bytes();
    let mut ev = vec![(
        0,
        Ev::Out(steam_page_at(scene.from.0, (&row, &row)).into_bytes()),
    )];
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
/// the Backspaces at its 15th; five rows down to the 290th column on a pane [`WIDE_COLS`] wide
/// after the Backspaces; 40 and 58 rows down from rest on the 85th row of a pane 160 rows tall,
/// where the window about the body reads rows far down the pane; and 88 rows up from rest at the
/// foot of a pane [`TALL_ROWS`] tall. The body gathers to leap where it rests fitted to its row,
/// far from the caret. From 600 ms, the cat and the dog, both ways of typing (a scene from rest
/// types no key, and runs once): no frame's box meets the letters the head law keeps it off
/// ([`kept_off`]: the row's beside the prompt's that the leaps do not cross, and for the leap up
/// the row above's while the box stands on the prompt's row), and none moves more than
/// [`STEP_ROWS`] rows from the frame before (measured at most 2.3). On a blank page the same
/// leaps find no letters, and no frame draws the body down its row or fits it: the rows read
/// about the body are read as they are, blank. Negative control (the law reverted alone, the cat
/// and the dog, both ways of typing): the map reads about the caret alone, the head law reads
/// none of the body's rows and lets its fit go as it gathers to leap, and its box meets those
/// letters in each scene's [`FarLeap::bites`] frames or more (measured on Linux arm64,
/// 2026-11-07: 38 or 39, 43 and 12 from rest; 9 or 10 at the window and 11 or 12 as the gallery
/// types after the Backspaces on the tall pane, 37 and 39 on the wide one; 38 or 39 and 43 on
/// the 160-row pane; 10 at the foot, standing on the prompt's row). In every scene the body
/// follows the caret away from the prompt's rows in 60 frames or more (90 to 308), and between
/// the lettered rows the head law holds it, drawn down or fitted, in 100 frames or more (153 to
/// 374).
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
                    for (lettered, control) in [(true, false), (false, false), (true, true)] {
                        runs.push((scene, cell, style, *ctl, lettered, control));
                    }
                }
            }
        }
    }
    let seen = aterm_spec::check::each(&runs, |&(scene, cell, style, ctl, lettered, control)| {
        let old: &[OldPath] = if control {
            &[OldPath::WorldReadsTheBodysRows]
        } else {
            &[]
        };
        let frames = far_leap(cell, style, (scene, lettered), old, ctl);
        let held: Vec<u64> = frames
            .iter()
            .filter(|f| f.ms >= FROM && (f.sink_fit.0 > 0.0 || f.sink_fit.1 < 1.0))
            .map(|f| f.ms)
            .collect();
        let prompt = usize::from(scene.from.0) - 1;
        (
            away_from_the_prompt(&frames, prompt, cell.1),
            kept_off(&frames, scene, cell.1),
            held,
            largest_step(&frames, cell.1),
        )
    });
    for (&(scene, cell, style, ctl, lettered, control), (away, met, held, step)) in
        runs.iter().zip(&seen)
    {
        let name = scene.name;
        assert!(
            *away >= 60,
            "fixture: {style} on {cell:?} ({name}, ctl {ctl}) follows the caret ({away} frames)"
        );
        assert!(
            !lettered || held.len() >= 100,
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
                let frames = far_leap(cell, style, (scene, true), old, false);
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
