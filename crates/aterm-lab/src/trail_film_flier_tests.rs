// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Nothing blinks and nothing pops: the flying cat's own animation and its moves, counted by the
//! film's own [`Turns`].
//!
//! Every frame is drawn by the window's own route: a real [`EffectsPipeline`] keeping the brand
//! cat's glide (`set_cursor_flier(Some(Cat))`) over a real [`Terminal`], fed each key as
//! `aterm-gui`'s input path feeds it (the cursor cat's own calls, the trail's licences and the
//! program's echo), applied every 16 ms, so the flier is routed by
//! [`EffectsPipeline::route_flying_head`] and placed by
//! [`EffectsPipeline::prepare_flying_head`] over the terminal's cells, and drawn by
//! [`EffectsPipeline::draw_flying_head`]. Every sprite it emits is resampled bilinearly into its
//! rect and composited, in linear light at the sprite's alpha, on a white page and on a dark
//! one. The film's flash law then reads each page: no pixel makes more than six turns in any
//! second. No frame moves the glide at full alpha by more than its stride ([`GLIDE_STRIDE`])
//! unless the frames beside it move it on too (a flight or an ease): a move drawn in one frame
//! is a pop. No frame draws the glide's body over a letter or the caret's cell, and none draws
//! it at full alpha under [`HEAD_FIT_FLOOR`] of its whole width. Typing with no other move
//! moves it only with the keys: no frame with the caret where it was moves it by more than a
//! pixel of breath, where a step the key could have taken at once was flown over frames. No
//! frame moves it along its row by more than a cell tail first, and none moves it up or down
//! only to take the move back within 120 ms.
//!
//! The script holds the caret still through most of it, so every turn a pixel makes there is
//! the flier's own: its breath, its face swaps, its dance, its one-shot reactions; then the caret
//! runs a cell a key at the film's burst speed. Each move a caret makes with no meteor behind it
//! is a scene of its own on its own page: a paste, a completion, a word kill, a short hop, a
//! two-row hop, typing with letters ahead of the caret, and typing into the right wall, under
//! blank rows, between full ones, under a screen of full rows, under two full rows and under a
//! stair of shorter lines; a program's lines printed under the prompt; Home and End back and
//! forth; and the paste as the PTY delivers it, in four reads.

use super::{FLASH_DARK_BELOW, FLASH_MOST, FLASH_SWING, PixelRead, Turns};
use aterm_core::terminal::{ClockReading, Rgb, Terminal};
use aterm_effects::cursor_fx::TrailColor;
use aterm_effects::cursor_glow::InsertWidth;
use aterm_effects::kitty_cursor::{
    CursorBody, FACE_LATCH_OFF, FLIER_CALM_OFF, GLIDE_WHIPS, SingSync,
};
use aterm_effects::kitty_pet::PetSpecies;
use aterm_effects::kitty_sing::SING_BPM;
use aterm_effects::pipeline::EffectsPipeline;
use aterm_effects::rainbow_kitty::companion::HEAD_FIT_FLOOR;
use aterm_effects::rainbow_kitty::companion::{
    FIT_BELOW_OFF, FIT_LATE, FIT_RISES_OFF, WALL_HOLD_OFF, WALL_LEAVE_OFF, WALL_RISE_OFF,
};
use aterm_effects::word_decorations::{
    FADE_OUT_ANYWHERE, FLIGHT_FACES_OFF, FLIGHT_UNPLANNED, GLIDE_FOLLOWS, GLIDE_JUMPS,
    GLIDE_SEATS_UNSTRIDED, GLIDE_STRIDE, GLIDE_STRIDE_OFF, LANDING_TURNS, REFLY_BACK,
    RETARGET_KEPT, TAKE_OFF_DRAWN,
};
use aterm_render::sprites::FreeSprite;

/// The film's frame step.
const STEP_MS: u32 = 16;
/// The script's caret, (row, col): it moves only in the run, a cell a key.
const CARET: (u16, u16) = (4, 12);
const ROWS: u16 = 8;
const COLS: u16 = 100;
/// The theme's caret colour (the film's dark look's).
const CURSOR: u32 = 0x0050_FA7B;
/// The pages: white and a dark theme's ground, each with its ink.
const PAGES: [(&str, u32, u32); 2] = [
    ("white", 0xFF_FF_FF, 0x1F_23_28),
    ("dark", 0x1A_1B_26, 0xC0_CA_F5),
];

/// What the script does at one instant, each as the window's input path hands it on.
#[derive(Clone, Copy, Debug)]
enum Ev {
    /// A key the cursor cat hears with the caret held still.
    Key,
    /// A typed letter the program echoes: the caret moves a cell to the right.
    Type,
    /// A typed space inserted before the letters ahead of the caret, which shift right with it.
    Insert,
    Delete,
    Enter,
    Curse,
    Delight,
    /// A paste of this many letters, echoed at once.
    Paste(u16),
    /// Tab, and the program's completion of this many letters.
    Complete(u16),
    /// Ctrl-W over a word of this many cells: the program steps back over it and erases it.
    WordKill(u16),
    /// A motion key the program answers by moving the caret this many cells left.
    Hop(u16),
    /// A motion key the program answers by moving the caret this many rows up.
    Rise(u16),
    /// A motion key (Home, End, Ctrl-A, Ctrl-E) the program answers with a caret move to (row,
    /// col).
    Cup(u16, u16),
    /// More of a paste, echoed with no gesture: a paste the PTY delivers over several reads.
    Chunk(u16),
    /// The program prints this many lines and its prompt below them.
    Lines(u16),
}

/// A run: the page the program draws before the flier comes (its caret's place included), the
/// script and its phases, in order, with how long each runs (ms). The first phase summons the
/// flier and is not judged by the move law; every later one has the glide on glass for most
/// of its frames.
struct Run {
    name: &'static str,
    page: String,
    phases: &'static [(&'static str, u32)],
    script: Vec<(u32, Ev)>,
    /// Typing whose every move of the glide is a key's: judged by the key law too.
    keyed: bool,
}

/// The script's phases.
const SCRIPT: [(&str, u32); 10] = [
    ("summon", 2_000),
    ("typing", 3_000),
    ("sing", 4_000),
    ("enter", 2_000),
    ("backspace", 2_000),
    ("wince", 1_500),
    ("delight", 1_500),
    ("flurry", 2_000),
    ("hover", 2_000),
    ("run", 3_500),
];

/// A move scene's phases: the flier summoned with the caret still, then the move and the
/// glide's flight or ease to it, held on glass by keys.
const SCENE: [(&str, u32); 2] = [("summon", 3_000), ("move", 2_500)];

/// A typing scene's phases: the summon, then the film's burst (66 keys 50 ms apart) and the
/// glide's settling after it.
const BURST_SCENE: [(&str, u32); 2] = [("summon", 3_000), ("move", 4_000)];

/// The CUP sequence to (row, col), 0-based.
fn at(row: u16, col: u16) -> String {
    format!("\x1b[{};{}H", row + 1, col + 1)
}

/// A row of `n` letters from (row, col).
fn letters(row: u16, col: u16, n: u16) -> String {
    let text: String = "the quick brown fox jumps over the lazy dog "
        .chars()
        .cycle()
        .take(usize::from(n))
        .map(|c| if c == ' ' { 'x' } else { c })
        .collect();
    format!("{}{text}", at(row, col))
}

/// The script's events: a summoning run at 40 keys a second, then 3 s at 11 keys a second; a
/// 4 s song at [`SING_BPM`] (typing on); Enter every 300 ms between keys; Backspace bursts at
/// 12 a second between runs of keys; one wince phrase and one delight phrase over typing; a
/// flurry of oops and Enter faster than a face can stand; a hover with no keys at all; then the
/// film's burst (67 keys 50 ms apart), each key moving the caret a cell.
fn script() -> Vec<(u32, Ev)> {
    let mut out = Vec::new();
    let mut from = 0;
    for &(name, len) in &SCRIPT {
        let end = from + len;
        match name {
            "summon" => (from..end).step_by(25).for_each(|t| out.push((t, Ev::Key))),
            "typing" | "sing" => (from..end).step_by(91).for_each(|t| out.push((t, Ev::Key))),
            "run" => (from..end)
                .step_by(50)
                .take(67)
                .for_each(|t| out.push((t, Ev::Type))),
            "enter" => {
                for t in (from..end).step_by(91) {
                    out.push((t, Ev::Key));
                }
                for t in (from..end).step_by(300) {
                    out.push((t + 45, Ev::Enter));
                }
            }
            "backspace" => {
                // Two deletes at 12 a second, then a quick run of keys, over and over. (Four
                // drain the cat's momentum under its leave gate: it fades and is revived, the
                // lifecycle's dip, not the glide's own motion.)
                let mut t = from;
                while t < end {
                    for _ in 0..2 {
                        out.push((t, Ev::Delete));
                        t += 83;
                    }
                    for _ in 0..16 {
                        out.push((t, Ev::Key));
                        t += 40;
                    }
                }
            }
            "flurry" => {
                // Oops and delight, faster than a face can stand: a delete and an Enter, each
                // every 300 ms and 150 ms apart, in a run at 40 keys a second.
                for t in (from..end).step_by(25) {
                    out.push((t + 5, Ev::Key));
                }
                for t in (from..end).step_by(300) {
                    out.extend([(t, Ev::Delete), (t + 150, Ev::Enter)]);
                }
            }
            "wince" | "delight" => {
                (from..end).step_by(91).for_each(|t| out.push((t, Ev::Key)));
                out.push((
                    from + 200,
                    if name == "wince" {
                        Ev::Curse
                    } else {
                        Ev::Delight
                    },
                ));
            }
            _ => {}
        }
        from = end;
    }
    out
}

/// A move scene's events over `phases`: the summoning run (40 keys a second, the caret still),
/// then `moves` from 200 ms into the move phase, and keys at 11 a second through the phase to
/// hold the flier.
fn scene_script(phases: &[(&str, u32)], moves: &[(u32, Ev)]) -> Vec<(u32, Ev)> {
    let (summon, len) = (phases[0].1, phases[1].1);
    let mut out: Vec<(u32, Ev)> = (0..summon).step_by(25).map(|t| (t, Ev::Key)).collect();
    out.extend((summon..summon + len).step_by(91).map(|t| (t, Ev::Key)));
    out.extend(moves.iter().map(|&(t, ev)| (summon + 200 + t, ev)));
    out
}

/// `n` of `ev` 50 ms apart, the film's burst speed.
fn burst(ev: Ev, n: u32) -> Vec<(u32, Ev)> {
    (0..n).map(|i| (i * 50, ev)).collect()
}

/// The runs: the script on a blank page, then each move with no meteor behind it.
fn runs() -> Vec<Run> {
    let prompt = |row: u16, text: &str| format!("{}$ {text}", at(row, 0));
    // Every even row full, the caret's row 5 between two of them lettered to 66 cells from the
    // wall's last column: no row holds the glide whole, and the film's burst types it to the
    // last column.
    let walled = format!(
        "{}{}{}{}{}",
        letters(0, 0, COLS),
        letters(2, 0, COLS),
        letters(4, 0, COLS),
        letters(6, 0, COLS),
        wall_row(5)
    );
    vec![
        Run {
            name: "script",
            page: at(CARET.0, CARET.1),
            phases: &SCRIPT,
            script: script(),
            keyed: false,
        },
        Run {
            name: "paste-40",
            page: prompt(4, "echo "),
            phases: &SCENE,
            script: scene_script(&SCENE, &[(0, Ev::Paste(40))]),
            keyed: false,
        },
        Run {
            name: "completion-30",
            page: prompt(4, "git ch"),
            phases: &SCENE,
            script: scene_script(&SCENE, &[(0, Ev::Complete(30))]),
            keyed: false,
        },
        Run {
            name: "word-kill",
            page: prompt(4, "echo the quick brown unbelievably"),
            phases: &SCENE,
            script: scene_script(&SCENE, &[(0, Ev::WordKill(12))]),
            keyed: false,
        },
        Run {
            name: "hop-5",
            page: prompt(4, "echo the quick brown fox jumps over"),
            phases: &SCENE,
            script: scene_script(&SCENE, &[(0, Ev::Hop(5))]),
            keyed: false,
        },
        Run {
            name: "hop-2-rows",
            page: format!(
                "{}{}",
                prompt(2, "echo the quick brown fox jumps over"),
                prompt(4, "echo the quick brown fox jumps over")
            ),
            phases: &SCENE,
            script: scene_script(&SCENE, &[(0, Ev::Rise(2))]),
            keyed: false,
        },
        Run {
            // An editor's line indented before its code: blank behind the caret, letters ahead.
            name: "letters-ahead",
            page: format!("{}{}", letters(4, 10, 50), at(4, 10)),
            phases: &BURST_SCENE,
            script: scene_script(&BURST_SCENE, &burst(Ev::Insert, 66)),
            keyed: true,
        },
        Run {
            // A prompt's line typed into the wall with blank rows above it: the glide rises off
            // the row to the sky seat as the room past the caret runs out.
            name: "open-wall",
            page: wall_row(4),
            phases: &BURST_SCENE,
            script: scene_script(&BURST_SCENE, &burst(Ev::Type, 66)),
            keyed: true,
        },
        Run {
            name: "right-wall",
            page: walled,
            phases: &BURST_SCENE,
            script: scene_script(&BURST_SCENE, &burst(Ev::Type, 66)),
            keyed: true,
        },
        Run {
            // A screen of output with the prompt at its foot: every row above the caret's full
            // to the wall, so by the wall no seat holds the glide, whole or fitted.
            name: "full-above-wall",
            page: format!(
                "{}{}",
                (0..ROWS - 1)
                    .map(|row| letters(row, 0, COLS))
                    .collect::<String>(),
                wall_row(ROWS - 1)
            ),
            phases: &BURST_SCENE,
            script: scene_script(&BURST_SCENE, &burst(Ev::Type, 66)),
            keyed: true,
        },
        Run {
            // The two rows above the caret's full to the wall, blank rows below it.
            name: "two-full-above-wall",
            page: format!(
                "{}{}{}",
                letters(3, 0, COLS),
                letters(4, 0, COLS),
                wall_row(5)
            ),
            phases: &BURST_SCENE,
            script: scene_script(&BURST_SCENE, &burst(Ev::Type, 66)),
            keyed: true,
        },
        Run {
            // A shell's lines over the caret's, each a cell or so shorter than the one below it
            // (`lab trail-film --scene wall-72`): typed into the wall, the seat steps up a row.
            name: "stair-wall",
            page: format!(
                "{}{}{}",
                letters(5, 0, COLS - 25),
                letters(6, 0, COLS - 7),
                wall_row(7)
            ),
            phases: &BURST_SCENE,
            script: scene_script(&BURST_SCENE, &burst(Ev::Type, 66)),
            keyed: true,
        },
        Run {
            // A program's lines printed under the prompt and a new prompt below them.
            name: "lines-down",
            page: prompt(0, "ls"),
            phases: &SCENE,
            script: scene_script(&SCENE, &[(0, Ev::Lines(6))]),
            keyed: false,
        },
        Run {
            // Home and End, back and forth along a line, 250 ms apart.
            name: "home-end",
            page: prompt(4, LINE_48),
            phases: &SCENE,
            script: scene_script(
                &SCENE,
                &[
                    (0, Ev::Cup(4, 2)),
                    (250, Ev::Cup(4, 50)),
                    (500, Ev::Cup(4, 2)),
                    (750, Ev::Cup(4, 50)),
                ],
            ),
            keyed: false,
        },
        Run {
            // The 40-cell paste as the PTY delivers it, in four reads 17 ms apart: the flight
            // retargets three times on its way.
            name: "paste-streamed",
            page: prompt(4, "echo "),
            phases: &SCENE,
            script: scene_script(
                &SCENE,
                &[
                    (0, Ev::Paste(10)),
                    (17, Ev::Chunk(10)),
                    (34, Ev::Chunk(10)),
                    (51, Ev::Chunk(10)),
                ],
            ),
            keyed: false,
        },
    ]
}

/// A 48-cell shell line.
const LINE_48: &str = "echo the quick brown fox jumps over the lazy dog";

/// The caret's `row` lettered to 67 cells from the wall's last column, the caret after them:
/// the film's burst types it to the last column.
fn wall_row(row: u16) -> String {
    format!("{}{}", letters(row, 0, COLS - 67), at(row, COLS - 67))
}

/// The phase of `phases` at `ms` and its start.
fn phase_at(phases: &[(&str, u32)], ms: u32) -> (usize, u32) {
    let mut from = 0;
    for (i, &(_, len)) in phases.iter().enumerate() {
        if ms < from + len {
            return (i, from);
        }
        from += len;
    }
    (phases.len() - 1, from)
}

fn total_ms(phases: &[(&str, u32)]) -> u32 {
    phases.iter().map(|&(_, len)| len).sum()
}

/// One frame's drawn sprites, whether the glide was on glass and at full alpha, the box the
/// pipeline names as the flier's body `(x0, x1, y0, y1)`, and the first cell holding a letter or
/// the caret that box lies on while the glide is on glass.
struct Shot {
    ms: u32,
    sprites: Vec<FreeSprite>,
    glide: bool,
    full: bool,
    body: Option<(i32, i32, i32, i32)>,
    meets: Option<(usize, usize)>,
    /// The caret's cell, (row, col).
    caret: (usize, usize),
}

const fn rgb(c: u32) -> Rgb {
    Rgb {
        r: (c >> 16) as u8,
        g: (c >> 8) as u8,
        b: c as u8,
    }
}

/// The window: a pipeline keeping the flier over a terminal whose program draws `page`, at
/// `cell_h` on `page`'s ground and ink.
struct Host {
    fx: EffectsPipeline,
    term: Terminal,
    cell: (u16, u16),
    ms: u32,
}

impl Host {
    fn new(cell_h: u16, (bg, fg): (u32, u32), page: &str) -> Self {
        let mut term = Terminal::new(ROWS, COLS);
        term.set_default_foreground(rgb(fg));
        term.set_default_background(rgb(bg));
        term.set_default_cursor_color(Some(rgb(CURSOR)));
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
            CURSOR,
        );
        fx.set_cursor_pet(false, 0x5EED);
        fx.set_cursor_flier(Some(PetSpecies::Cat));
        // The body the pet setting gives the cursor cat, before its first key.
        let cat = &mut fx.companions_mut().cursor_cat;
        cat.set_body(Some(PetSpecies::Cat));
        cat.set_look(aterm_effects::kitty_registry::KittyLook::DEFAULT);
        let mut host = Self {
            fx,
            term,
            cell: ((cell_h / 2).max(1), cell_h),
            ms: 0,
        };
        host.out(page.as_bytes());
        host
    }

    fn to(&mut self, ms: u32) {
        if ms > self.ms {
            self.fx.advance(f64::from(ms - self.ms));
            self.ms = ms;
        }
    }

    /// The program's output.
    fn out(&mut self, bytes: &[u8]) {
        let at = ClockReading {
            monotonic: self.fx.now(),
            wall_ms: None,
        };
        self.term.process_at(bytes, at);
    }

    /// A typed scalar, as the window commits it, and the program's echo of `echo`.
    fn typed(&mut self, ch: char, echo: &str) {
        let input = self.term.cell_frame(usize::from(ROWS), usize::from(COLS));
        let _ = self.fx.note_committed_char(&mut self.term, &input, ch);
        self.out(echo.as_bytes());
    }

    /// A motion key, as the window hands it to the trail before the program answers it.
    fn nav(&mut self, now: std::time::Instant) {
        self.fx.note_keystroke();
        self.fx.cursor_mut().glow.note_motion(now);
        self.fx.cursor_mut().trail.note_navigation(now);
    }

    /// One event, as `aterm-gui`'s input path hands it to the cursor cat and the trail.
    fn on(&mut self, ev: Ev) {
        let now = self.fx.now();
        let letters_of = |n: u16| "abcdefghij".chars().cycle().take(usize::from(n));
        match ev {
            Ev::Key => self.fx.companions_mut().cursor_cat.on_key(now, true),
            Ev::Type => self.typed('x', "x"),
            Ev::Insert => self.typed(' ', "\x1b[@ "),
            Ev::Delete => self.fx.companions_mut().cursor_cat.on_key(now, false),
            Ev::Enter => self.fx.companions_mut().cursor_cat.on_enter(now),
            Ev::Curse => assert!(
                self.fx.companions_mut().cursor_cat.on_curse(now, 1),
                "the flier was not on glass to wince at {} ms",
                self.ms
            ),
            Ev::Delight => assert!(
                self.fx.companions_mut().cursor_cat.on_delight(now, 1),
                "the flier was not on glass to delight at {} ms",
                self.ms
            ),
            Ev::Paste(n) => {
                let glow = &mut self.fx.cursor_mut().glow;
                glow.note_user_gesture(now);
                let _ = glow.cue_paste(now);
                self.fx.cursor_mut().trail.note_user_gesture(now);
                self.out(letters_of(n).collect::<String>().as_bytes());
            }
            Ev::Complete(n) => {
                self.typed('\t', "");
                let glow = &mut self.fx.cursor_mut().glow;
                glow.note_user_gesture(now);
                glow.note_insert_armed(now, InsertWidth::Unknown);
                self.fx.cursor_mut().trail.note_user_gesture(now);
                self.out(letters_of(n).collect::<String>().as_bytes());
            }
            Ev::WordKill(n) => {
                self.fx.note_keystroke();
                self.fx.cursor_mut().glow.note_word_kill(now, true);
                self.fx.companions_mut().cursor_cat.on_kill(now);
                self.out(format!("{}\x1b[K", "\x08".repeat(usize::from(n))).as_bytes());
            }
            Ev::Hop(n) | Ev::Rise(n) => {
                self.nav(now);
                let dir = if matches!(ev, Ev::Hop(_)) { 'D' } else { 'A' };
                self.out(format!("\x1b[{n}{dir}").as_bytes());
            }
            Ev::Cup(row, col) => {
                self.nav(now);
                self.out(at(row, col).as_bytes());
            }
            Ev::Chunk(n) => self.out(letters_of(n).collect::<String>().as_bytes()),
            Ev::Lines(n) => {
                let lines: String = (0..n).map(|i| format!("\r\nline {i}")).collect();
                self.out(format!("{lines}\r\n$ ").as_bytes());
            }
        }
    }

    /// Present one frame: the flier's sprites as the pipeline drew them.
    fn shot(&mut self) -> Shot {
        let (cw, ch) = self.cell;
        let mut input = self.term.cell_frame(usize::from(ROWS), usize::from(COLS));
        let page = input.cells.clone();
        let caret = (input.cursor_row, input.cursor_col);
        self.fx
            .apply(&mut self.term, &mut input, usize::from(cw), usize::from(ch));
        let frame = self.fx.cursor_flier_frame();
        let glide = frame.is_some_and(|f| f.alpha > 0 && matches!(f.body, CursorBody::Glide(_)));
        let body = self.fx.cursor().glow.head_body_window_box();
        let (cw, ch) = (i32::from(cw), i32::from(ch));
        let meets = body.filter(|_| glide).and_then(|(x0, x1, y0, y1)| {
            let cells = |a: i32, b: i32, size: i32, n: u16| {
                let first = (a.max(0) / size) as usize;
                let last = ((b - 1).max(0) / size).min(i32::from(n) - 1) as usize;
                first..=last
            };
            cells(y0, y1, ch, ROWS).find_map(|r| {
                cells(x0, x1, cw, COLS)
                    .find(|&c| {
                        (r, c) == caret
                            || page
                                .get(r)
                                .and_then(|row| row.get(c))
                                .is_some_and(|cell| cell.ch != ' ')
                    })
                    .map(|c| (r, c))
            })
        });
        // The pet is off and the words are off: the frame's free sprites are the flier's.
        let sprites = self.fx.free_sprites().to_vec();
        Shot {
            ms: self.ms,
            glide,
            // At full alpha as drawn, a flight's fade included.
            full: glide && sprites.iter().any(|s| s.alpha == u8::MAX),
            sprites,
            body,
            meets,
            caret,
        }
    }
}

/// Fly `run` at `cell_h` on `page` (its ground and ink) and keep every frame.
fn fly(run: &Run, cell_h: u16, page: (u32, u32)) -> Vec<Shot> {
    let mut host = Host::new(cell_h, page, &run.page);
    let mut events = run.script.clone();
    events.retain(|&(t, _)| t < total_ms(run.phases));
    events.sort_by_key(|&(t, _)| t);
    let sing = run.phases.iter().position(|&(n, _)| n == "sing");
    let mut next = 0;
    let mut shots = Vec::new();
    let mut ms = 0;
    while ms < total_ms(run.phases) {
        while next < events.len() && events[next].0 <= ms {
            host.to(events[next].0);
            host.on(events[next].1);
            next += 1;
        }
        host.to(ms);
        let (phase, from) = phase_at(run.phases, ms);
        let sync = if Some(phase) == sing {
            SingSync {
                drive: 1.0,
                beat: (ms - from) as f32 / 1000.0 * SING_BPM / 60.0,
            }
        } else {
            SingSync::NONE
        };
        let now = host.fx.now();
        host.fx.companions_mut().cursor_cat.set_singing(now, sync);
        shots.push(host.shot());
        ms += STEP_MS;
    }
    shots
}

/// An sRGB channel in linear light, by the exact curve.
fn lin(c: u8) -> f32 {
    let s = f32::from(c) / 255.0;
    if s <= 0.040_45 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

/// `rgb` (0xRRGGBB) in linear light.
fn lin_rgb(rgb: u32) -> [f32; 3] {
    [
        lin((rgb >> 16) as u8),
        lin((rgb >> 8) as u8),
        lin(rgb as u8),
    ]
}

/// One sprite's art, premultiplied in linear light, `w x h`.
struct Art {
    w: usize,
    h: usize,
    px: Vec<[f32; 4]>,
}

impl Art {
    fn of(sprite: &FreeSprite) -> Self {
        let (w, h) = (usize::from(sprite.art.w()), usize::from(sprite.art.h()));
        let tint = lin_rgb(sprite.tint);
        let data = sprite.art.data();
        let px = (0..w * h)
            .map(|i| {
                let t = &data[i * 4..i * 4 + 4];
                let a = f32::from(t[3]) / 255.0 * f32::from(sprite.alpha) / 255.0;
                [
                    lin(t[0]) * tint[0] * a,
                    lin(t[1]) * tint[1] * a,
                    lin(t[2]) * tint[2] * a,
                    a,
                ]
            })
            .collect();
        Self { w, h, px }
    }

    fn at(&self, x: i64, y: i64) -> [f32; 4] {
        if x < 0 || y < 0 || x >= self.w as i64 || y >= self.h as i64 {
            return [0.0; 4];
        }
        self.px[y as usize * self.w + x as usize]
    }

    /// Bilinear, transparent past the edge.
    fn sample(&self, u: f32, v: f32) -> [f32; 4] {
        let (x0, y0) = (u.floor(), v.floor());
        let (fx, fy) = (u - x0, v - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let mut out = [0.0; 4];
        for (dx, dy, k) in [
            (0, 0, (1.0 - fx) * (1.0 - fy)),
            (1, 0, fx * (1.0 - fy)),
            (0, 1, (1.0 - fx) * fy),
            (1, 1, fx * fy),
        ] {
            let t = self.at(x0 + dx, y0 + dy);
            for c in 0..4 {
                out[c] += t[c] * k;
            }
        }
        out
    }
}

/// The union of every sprite's rect over the run, in grid pixels (x0, y0, x1, y1).
fn reach(shots: &[Shot]) -> (i32, i32, i32, i32) {
    let mut r = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for s in shots.iter().flat_map(|s| &s.sprites) {
        r = (
            r.0.min(s.x),
            r.1.min(s.y),
            r.2.max(s.x + i32::from(s.w)),
            r.3.max(s.y + i32::from(s.h)),
        );
    }
    r
}

/// A frame whose full-alpha glide moved more than its stride with no frame beside it moving it
/// on by half as much: `(ms, px)`.
type Pop = (u32, i32);

/// The flash law's reading of `shots` on a page of `bg`, over the whole run and phase by
/// phase, the move law's pops, the frames whose glide lies on a letter or the caret (`(ms,
/// (row, col))`), and the least width the full-alpha glide is drawn at, px.
struct Reading {
    whole: PixelRead,
    phases: Vec<PixelRead>,
    glide_frames: Vec<usize>,
    pops: Vec<Pop>,
    unkeyed: Vec<Pop>,
    on_letters: Vec<(u32, (usize, usize))>,
    least: i32,
    tail_first: Vec<Pop>,
    bounces: Vec<Pop>,
    most_step: i32,
}

/// The move law over `shots` at a `cell_w` px cell: the full-alpha glide's every step from one
/// frame to the next, the greater of its middle's moves along and across; a step of more than
/// the stride ([`GLIDE_STRIDE`] cells) is a pop unless a step beside it is half its size or
/// more, as a flight's and an ease's are. The first phase (the summon) is not judged.
fn pops(phases: &[(&str, u32)], shots: &[Shot], cell_w: u16) -> Vec<Pop> {
    let stride = i32::from(GLIDE_STRIDE) * i32::from(cell_w);
    let steps: Vec<i32> = shots
        .windows(2)
        .map(|w| match (middle(&w[0]), middle(&w[1])) {
            // Twice the middle: halve it.
            (Some(a), Some(b)) => ((b.0 - a.0).abs().max((b.1 - a.1).abs()) + 1) / 2,
            _ => 0,
        })
        .collect();
    let mut out = Vec::new();
    for (i, &step) in steps.iter().enumerate() {
        let ms = shots[i + 1].ms;
        if phase_at(phases, ms).0 == 0 || step <= stride {
            continue;
        }
        let before = if i > 0 { steps[i - 1] } else { 0 };
        let after = steps.get(i + 1).copied().unwrap_or(0);
        if before * 2 < step && after * 2 < step {
            out.push((ms, step));
        }
    }
    out
}

/// The key law over `shots`: a frame after the first phase whose full-alpha glide moved, by more
/// than a pixel of breath along or across, from the frame before with the caret where it was
/// there, `(ms, px)`: a move no key made, the middle or the end of a flight a key could have
/// taken at once.
fn unkeyed(phases: &[(&str, u32)], shots: &[Shot]) -> Vec<Pop> {
    shots
        .windows(2)
        .filter(|w| phase_at(phases, w[1].ms).0 > 0 && w[0].caret == w[1].caret)
        .filter_map(|w| match (middle(&w[0]), middle(&w[1])) {
            (Some(a), Some(b)) => {
                let px = ((b.0 - a.0).abs().max((b.1 - a.1).abs()) + 1) / 2;
                (px > 1).then_some((w[1].ms, px))
            }
            _ => None,
        })
        .collect()
}

/// The full-alpha glide's middle in `shot`, doubled.
fn middle(s: &Shot) -> Option<(i32, i32)> {
    s.body
        .filter(|_| s.full)
        .map(|(x0, x1, y0, y1)| (x0 + x1, y0 + y1))
}

/// The facing law over `shots` at a `cell_w` px cell: a frame after the first phase whose
/// full-alpha glide moved along its row by more than a cell, tail first (left with its art
/// unflipped, or right flipped), `(ms, px)`. A flight goes head first.
fn tail_first(phases: &[(&str, u32)], shots: &[Shot], cell_w: u16) -> Vec<Pop> {
    shots
        .windows(2)
        .filter(|w| phase_at(phases, w[1].ms).0 > 0)
        .filter_map(|w| {
            let (a, b) = (middle(&w[0])?, middle(&w[1])?);
            let dx = (b.0 - a.0) / 2;
            let flipped = w[1].sprites.iter().any(|s| s.alpha == u8::MAX && s.flip_x);
            (dx.abs() > i32::from(cell_w) && (dx < 0) != flipped).then_some((w[1].ms, dx))
        })
        .collect()
}

/// The wobble law over `shots`: a full-alpha move up or down of more than a pixel of breath
/// (2 px) taken back the other way within 120 ms of it, `(ms, px)` of the move back. A seat that
/// comes down for one step by the wall and goes back up the next is a wobble.
fn bounces(phases: &[(&str, u32)], shots: &[Shot]) -> Vec<Pop> {
    let moves: Vec<Pop> = shots
        .windows(2)
        .filter(|w| phase_at(phases, w[1].ms).0 > 0)
        .filter_map(|w| {
            let dy = (middle(&w[1])?.1 - middle(&w[0])?.1) / 2;
            (dy.abs() > 2).then_some((w[1].ms, dy))
        })
        .collect();
    moves
        .windows(2)
        .filter(|m| m[1].0 - m[0].0 <= 120 && m[0].1.signum() != m[1].1.signum())
        .map(|m| m[1])
        .collect()
}

/// The full-alpha glide's largest step from one frame to the next after the first phase, px.
fn most_step(phases: &[(&str, u32)], shots: &[Shot]) -> i32 {
    shots
        .windows(2)
        .filter(|w| phase_at(phases, w[1].ms).0 > 0)
        .filter_map(|w| {
            let (a, b) = (middle(&w[0])?, middle(&w[1])?);
            Some(((b.0 - a.0).abs().max((b.1 - a.1).abs()) + 1) / 2)
        })
        .max()
        .unwrap_or(0)
}

/// The least width the full-alpha glide is drawn at in `shots`, px.
fn least_width(shots: &[Shot]) -> Option<i32> {
    shots
        .iter()
        .filter(|s| s.full)
        .filter_map(|s| s.body.map(|(x0, x1, ..)| x1 - x0))
        .min()
}

/// The glide's whole width at `cell_h`, px: its least at full alpha over a blank page.
fn whole_width(cell_h: u16) -> i32 {
    let run = Run {
        name: "whole",
        page: at(CARET.0, CARET.1),
        phases: &[("summon", 3_000)],
        script: (0..3_000).step_by(25).map(|t| (t, Ev::Key)).collect(),
        keyed: false,
    };
    least_width(&fly(&run, cell_h, (PAGES[0].1, PAGES[0].2))).expect("the glide came on glass")
}

/// Whether two frames' sprites composite to the same pixels: the same arts, by their texels, at
/// the same places, sizes and clips, with the same tints, alphas and facings, in the same order.
fn same_drawing(a: &[FreeSprite], b: &[FreeSprite]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(s, t)| {
            (s.x, s.y, s.w, s.h, s.clip, s.tint, s.alpha, s.flip_x)
                == (t.x, t.y, t.w, t.h, t.clip, t.tint, t.alpha, t.flip_x)
                && (std::sync::Arc::ptr_eq(&s.art, &t.art)
                    || (s.art.w(), s.art.h(), s.art.data()) == (t.art.w(), t.art.h(), t.art.data()))
        })
}

fn read(phases: &[(&str, u32)], shots: &[Shot], bg: u32, cell_w: u16) -> Reading {
    let (x0, y0, x1, y1) = reach(shots);
    assert!(x1 > x0 && y1 > y0, "the flier drew nothing");
    let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
    let bg = lin_rgb(bg);
    let mut whole = Turns::new(w * h, FLASH_SWING, FLASH_DARK_BELOW);
    let mut each: Vec<Turns> = phases
        .iter()
        .map(|_| Turns::new(w * h, FLASH_SWING, FLASH_DARK_BELOW))
        .collect();
    let mut glide_frames = vec![0; phases.len()];
    let mut canvas = vec![[0.0_f32; 3]; w * h];
    // Each pixel's luminance on the frame before, that frame's phase and the sprites last
    // composited. A pixel that kept its luminance turned nothing ([`Turns::see`] of the value it
    // saw last changes nothing), so, as the film's own `Flicker::frame` reads its frames, each
    // count sees a pixel only where it changed: the whole run's after its first frame, a phase's
    // after the phase's first frame. A frame that draws what the frame before drew
    // ([`same_drawing`]) keeps every pixel, and is composited only for nothing.
    let mut last = vec![0.0_f64; w * h];
    let mut last_phase = None;
    let mut drawn: Option<&[FreeSprite]> = None;
    for shot in shots {
        let (phase, _) = phase_at(phases, shot.ms);
        glide_frames[phase] += usize::from(shot.glide);
        let (first, same_phase) = (last_phase.is_none(), last_phase == Some(phase));
        last_phase = Some(phase);
        if drawn.is_some_and(|drawn| same_drawing(drawn, &shot.sprites)) {
            if !same_phase {
                for (i, &y) in last.iter().enumerate() {
                    each[phase].see(i, y, shot.ms);
                }
            }
            continue;
        }
        drawn = Some(&shot.sprites);
        canvas.fill(bg);
        for s in &shot.sprites {
            let art = Art::of(s);
            let (kx, ky) = (art.w as f32 / f32::from(s.w), art.h as f32 / f32::from(s.h));
            for py in s.y.max(y0)..(s.y + i32::from(s.h)).min(y1) {
                for px in s.x.max(x0)..(s.x + i32::from(s.w)).min(x1) {
                    if px < s.clip.x0 || px >= s.clip.x1 || py < s.clip.y0 || py >= s.clip.y1 {
                        continue;
                    }
                    let mut u = (px - s.x) as f32 + 0.5;
                    if s.flip_x {
                        u = f32::from(s.w) - u;
                    }
                    let v = (py - s.y) as f32 + 0.5;
                    let t = art.sample(u * kx - 0.5, v * ky - 0.5);
                    let i = (py - y0) as usize * w + (px - x0) as usize;
                    let c = &mut canvas[i];
                    for k in 0..3 {
                        c[k] = t[k] + c[k] * (1.0 - t[3]);
                    }
                }
            }
        }
        for (i, c) in canvas.iter().enumerate() {
            let y = f64::from(0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]);
            if !first && y == last[i] {
                if !same_phase {
                    each[phase].see(i, y, shot.ms);
                }
                continue;
            }
            last[i] = y;
            whole.see(i, y, shot.ms);
            each[phase].see(i, y, shot.ms);
        }
    }
    Reading {
        whole: whole.read(STEP_MS),
        phases: each.iter().map(|t| t.read(STEP_MS)).collect(),
        glide_frames,
        pops: pops(phases, shots, cell_w),
        unkeyed: unkeyed(phases, shots),
        on_letters: shots
            .iter()
            .filter_map(|s| s.meets.map(|m| (s.ms, m)))
            .collect(),
        least: least_width(shots).unwrap_or(i32::MAX),
        tail_first: tail_first(phases, shots, cell_w),
        bounces: bounces(phases, shots),
        most_step: most_step(phases, shots),
    }
}

fn report(label: &str, phases: &[(&str, u32)], r: &Reading) -> String {
    let mut out = format!(
        "{label}: over={} most={} turns={} pops={:?} unkeyed={:?} on_letters={} first={:?} \
         least_px={} tail_first={:?} bounces={:?} most_step={} worst={:?}",
        r.whole.over.len(),
        r.whole.most,
        r.whole.turns,
        r.pops,
        r.unkeyed,
        r.on_letters.len(),
        r.on_letters.first(),
        r.least,
        r.tail_first,
        r.bounces,
        r.most_step,
        r.whole.worst.as_ref().map(|w| (w.0, &w.2))
    );
    for (i, p) in r.phases.iter().enumerate() {
        out += &format!(
            " | {} over={} turns={} frames={} worst={:?}",
            phases[i].0,
            p.over.len(),
            p.turns,
            r.glide_frames[i],
            p.worst.as_ref().map(|w| (w.0, &w.2))
        );
    }
    out
}

/// The fit's floor at `cell_h`, to the pixel its box rounds to: [`HEAD_FIT_FLOOR`] of the glide's
/// whole width, read with the control seams as they stand.
fn floor_px(cell_h: u16) -> i32 {
    (HEAD_FIT_FLOOR * whole_width(cell_h) as f32).floor() as i32
}

/// Whether `run` at `cell_h` on `page` keeps the laws: some pixel turning more than six times a
/// second, a pop, a frame on a letter or the caret, a full-alpha glide narrower than `floor`
/// (its fit's floor at `cell_h`, [`floor_px`], read under the same seams), a step along its row
/// tail first, or a wobble breaks them. The reading's line, and the glide's largest step.
fn judged(run: &Run, cell_h: u16, page: (&str, u32, u32), floor: i32) -> Judged {
    let (name, bg, fg) = page;
    let shots = fly(run, cell_h, (bg, fg));
    let r = read(run.phases, &shots, bg, (cell_h / 2).max(1));
    let line = report(&format!("{} {name} {cell_h}px", run.name), run.phases, &r);
    for (i, &n) in r.glide_frames.iter().enumerate().skip(1) {
        assert!(
            n * 2 >= (run.phases[i].1 / STEP_MS) as usize,
            "{}: the glide must be on glass for most of the phase: {line}",
            run.phases[i].0
        );
    }
    let line = format!("{line} floor_px={floor}");
    let kept = r.whole.over.is_empty()
        && r.pops.is_empty()
        && (!run.keyed || r.unkeyed.is_empty())
        && r.on_letters.is_empty()
        && r.least >= floor
        && r.tail_first.is_empty()
        && r.bounces.is_empty();
    Judged {
        kept,
        line,
        most_step: r.most_step,
    }
}

/// One run's judgement at one cell height on one page ([`judged`]).
#[derive(Debug)]
struct Judged {
    kept: bool,
    line: String,
    most_step: i32,
}

/// The flier keeps the flash law and the move law on a white page and a dark one, at a 26 px
/// and a 20 px cell, every frame routed as the window routes it: through a script that types,
/// sings, presses Enter, deletes in bursts, winces, delights, swaps faces in a flurry, hovers and
/// then runs the caret along its row at 20 keys a second; and through every move a caret makes
/// with no meteor behind it (a 40-cell paste, a 30-cell completion, a word kill, a 5-cell hop,
/// a two-row hop, typing with letters ahead of the caret, typing into the right wall under blank
/// rows, between full ones, under a screen of full rows, under two full rows and under a stair of
/// shorter lines, a program's lines printed under the prompt, Home and End 250 ms apart, the
/// paste in four reads). No pixel makes more than six turns in any second, no frame moves the
/// full-alpha glide more than its stride unless it flies or eases there, no frame lays it on a
/// letter or the caret, none draws it at full alpha under its fit's floor, none moves it along
/// its row tail first, none takes a move up or down back within 120 ms, and along the typed runs
/// (letters ahead, the walls) no frame between keys moves it more than a pixel. The paste in four
/// reads steps no further a frame than the paste at once. After each run's summon, the glide is
/// on glass for most of every phase, so none is read vacuously.
///
/// Controls, each on the script in the same run: with the glide's calm off (the head's
/// three-level stride bob, squash and stretch, lean and recoil, and its every-beat dance), with
/// the face latch off (a face follows every reaction at once), with the follower easing the
/// glide after its rest as it eases the head, and with the glide moving a cell a key instead of
/// its two-cell stride, some pixel turns more than six times a second. Each move has its own,
/// 0a12236dc7b's behaviour back, under which some reading of it breaks a law: its flight
/// unplanned, the glide drawn at its rest, its seat behind the caret a cell a key, its take-off
/// frame drawn where it was (typed up the stair), no rise with its last stride step into the wall (with its fade out
/// held over letters, the other way that holds it there), and its fit by the wall shrinking, late
/// or a cell a key. The moves CT-56's second review found have theirs, the round before's
/// behaviour back: by a wall with no seat (a screen of full rows; two full rows, with the seat
/// below them, the leave and the fade out only where it is clear all taken away, each of which
/// holds it alone) the fit shrinks onto the letters or the caret; by the stair, the seat comes
/// down for one stride step and goes back up; the fade held out where the take-off lay over
/// letters leaves the glide over the program's new lines; every way back flown, and the meteor's
/// whip on the glide, each flash Home and End; flights tail first break the facing law; a flight
/// turned back on the frame it lands flashes the word kill; and the retargeted flight's plan kept
/// steps the streamed paste further than the paste at once.
#[test]
fn the_flier_never_flashes() {
    let runs = runs();
    let mut broken = Vec::new();
    // Each run's largest step at each cell height and page, in order.
    let mut most_steps = std::collections::HashMap::<&str, Vec<i32>>::new();
    // The fit's floor at each cell height, read once with no control seam on.
    let floors = [26_u16, 20].map(|cell_h| (cell_h, floor_px(cell_h)));
    for run in &runs {
        for (cell_h, floor) in floors {
            for &page in &PAGES {
                let j = judged(run, cell_h, page, floor);
                eprintln!("{}", j.line);
                most_steps.entry(run.name).or_default().push(j.most_step);
                if !j.kept {
                    broken.push(j.line);
                }
            }
        }
    }
    assert!(
        broken.is_empty(),
        "{} runs turn some pixel more than {} times a second, pop, meet a letter or the caret, or \
         shrink under the floor:\n{}",
        broken.len(),
        2 * FLASH_MOST,
        broken.join("\n")
    );

    let control = |off: &'static std::thread::LocalKey<std::cell::Cell<bool>>, label: &str| {
        off.with(|c| c.set(true));
        let Judged { kept, line, .. } = judged(&runs[0], 26, PAGES[0], floor_px(26));
        off.with(|c| c.set(false));
        eprintln!("{label}: {line}");
        assert!(
            !kept,
            "control: {label} must turn some pixel more than six times a second: {line}"
        );
    };
    control(&FLIER_CALM_OFF, "calm off");
    control(&FACE_LATCH_OFF, "latch off");
    control(&GLIDE_FOLLOWS, "follower on");
    control(&GLIDE_STRIDE_OFF, "a cell a key");

    // Each move's own control, 0a12236dc7b's behaviour back: some reading of the move breaks a
    // law.
    // With `offs` on, the cell heights and pages in the sweep's order (each height's floor read
    // under them), up to the first reading `broke` names (its index in that order): a control
    // holds once one reading breaks, and one that fails has read every one. `Err` holds them.
    let first_broken = |name: &str,
                        offs: &[Seam],
                        label: &str,
                        broke: &dyn Fn(usize, &Judged) -> bool|
     -> Result<(), Vec<Judged>> {
        let run = runs.iter().find(|r| r.name == name).expect("the run");
        offs.iter().for_each(|off| off.with(|c| c.set(true)));
        let mut read = Vec::new();
        let mut floor_at: Option<(u16, i32)> = None;
        let mut found = false;
        let readings = [26_u16, 20]
            .into_iter()
            .flat_map(|cell_h| PAGES.iter().map(move |&page| (cell_h, page)));
        for (k, (cell_h, page)) in readings.enumerate() {
            let floor = match floor_at {
                Some((h, px)) if h == cell_h => px,
                _ => floor_at.insert((cell_h, floor_px(cell_h))).1,
            };
            let j = judged(run, cell_h, page, floor);
            eprintln!("{label}: {}", j.line);
            found = broke(k, &j);
            read.push(j);
            if found {
                break;
            }
        }
        offs.iter().for_each(|off| off.with(|c| c.set(false)));
        if found { Ok(()) } else { Err(read) }
    };
    let moved_under = |name: &str, offs: &[Seam], label: &str| {
        if let Err(lines) = first_broken(name, offs, label, &|_, j| !j.kept) {
            panic!("control: {name} with {label} must break a law: {lines:?}");
        }
    };
    let moved = |name: &str, off: Seam, label: &str| moved_under(name, &[off], label);
    moved("paste-40", &FLIGHT_UNPLANNED, "the flight unplanned");
    for name in ["completion-30", "word-kill", "hop-5", "hop-2-rows"] {
        moved(name, &GLIDE_JUMPS, "the glide drawn at its rest");
    }
    moved(
        "letters-ahead",
        &GLIDE_SEATS_UNSTRIDED,
        "the seat behind a cell a key",
    );
    // Typed up the stair, the glide's seat climbs off its row in flights a key at a time.
    moved(
        "stair-wall",
        &TAKE_OFF_DRAWN,
        "the take-off frame drawn where it was",
    );
    // Its rise with its last stride step and its fade out only where it is clear each hold it.
    moved_under(
        "open-wall",
        &[&WALL_RISE_OFF, &FADE_OUT_ANYWHERE],
        "no rise with the last stride step and the fade held out over letters",
    );
    moved(
        "right-wall",
        &FIT_RISES_OFF,
        "the fit shrinking by the wall",
    );
    moved("right-wall", &FIT_LATE, "the fit from the next frame");
    moved("right-wall", &GLIDE_SEATS_UNSTRIDED, "the fit a cell a key");
    // Each of CT-56's second review's moves with its own control, the round before's behaviour
    // back.
    moved(
        "full-above-wall",
        &WALL_LEAVE_OFF,
        "the fit shrunk onto a wall with no seat",
    );
    // Each of its seat below, its leave and its fade out only where it is clear holds it alone.
    moved_under(
        "two-full-above-wall",
        &[&WALL_LEAVE_OFF, &FIT_BELOW_OFF, &FADE_OUT_ANYWHERE],
        "no seat below, the fit shrunk and the fade held out over letters",
    );
    moved(
        "stair-wall",
        &WALL_HOLD_OFF,
        "the seat down for one stride step",
    );
    moved(
        "lines-down",
        &FADE_OUT_ANYWHERE,
        "the fade held out only over letters",
    );
    moved("home-end", &REFLY_BACK, "each way back flown");
    moved("home-end", &GLIDE_WHIPS, "the meteor's whip on the glide");
    moved("home-end", &FLIGHT_FACES_OFF, "its flights tail first");
    moved(
        "word-kill",
        &LANDING_TURNS,
        "turned back on the frame it lands",
    );
    // A flight retargeted on its way steps no further a frame than the same move made at once.
    let steps: Vec<(i32, i32)> = most_steps["paste-40"]
        .iter()
        .copied()
        .zip(most_steps["paste-streamed"].iter().copied())
        .collect();
    assert!(
        steps.iter().all(|&(once, streamed)| streamed <= once),
        "the streamed paste's flight steps further a frame than the paste's (once, streamed): \
         {steps:?}"
    );
    let once = &most_steps["paste-40"];
    if let Err(lines) = first_broken(
        "paste-streamed",
        &[&RETARGET_KEPT],
        "the retargeted flight's plan kept",
        &|k, j| j.most_step > once[k],
    ) {
        let grown: Vec<(i32, i32)> = once
            .iter()
            .copied()
            .zip(lines.iter().map(|j| j.most_step))
            .collect();
        panic!(
            "control: with the retargeted flight's plan kept, the streamed paste must step \
             further (once, streamed): {grown:?}"
        );
    }
}

/// A control seam.
type Seam = &'static std::thread::LocalKey<std::cell::Cell<bool>>;
