// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ONE DISTANCE FROM THE CARET, NOT TWO.
//!
//! The caret's escort seats the pet `STATION_LEAD` past the caret — one
//! cell, the shipped v0.76.0 law. The console layer's RESIDENT — the pet
//! watching a running command's output from beside a parked caret — used
//! to reach the same caret through its own home rule, which added
//! `HOME_BREATHING_ROOM` (two more cells) to the station: the escort stood
//! at `caret + 1` and the resident at `caret + 3`, and the owner saw the cat
//! change its mind about where "beside me" is whenever a command started
//! printing. The escort's distance is the only precedent (v0.76.0 had no
//! resident path at all), so the resident's home is now the escort's stand.
//!
//! THE ORACLE IS THE PET ITSELF, as in `pet_escort_primacy` and
//! `pet_position_authority`: `set_console_presentable(false)` returns every
//! console entry point early, so a twin brain driven with it off IS the
//! v0.76.0 escort. The fixture is `pet_position_authority`'s streaming
//! case with a shell-integration block left EXECUTING, which is what makes
//! the live brain take the resident path.

use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;
use aterm_effects::kitty_pet::{PetAttention, PetBrain, PetFrame, PetSense, art_cols};
use aterm_effects::pet_world::{PetPane, PetRect, PetWorld, PetWorldFacts};

const ROWS: u16 = 24;
const COLS: u16 = 80;
const CELL_W: u16 = 8;
const CELL_H: u16 = 16;

struct Scene {
    term: Terminal,
    brain: PetBrain,
    presentable: bool,
    /// Feed the older ink seam the way the host does (`app_render.rs`
    /// hands `pet_ink()` to `sense_ink` every frame).
    ink: bool,
}

impl Scene {
    /// `presentable == false` is the v0.76.0 escort with no console layer.
    fn new(presentable: bool, ink: bool, home: &[u8]) -> Self {
        let mut term = Terminal::new(ROWS, COLS);
        term.process(b"\x1b]11;#000000\x07");
        term.process(home);
        Self {
            term,
            brain: PetBrain::default(),
            presentable,
            ink,
        }
    }

    fn tick(&mut self, now: Instant) -> PetFrame {
        let input = self.term.cell_frame(usize::from(ROWS), usize::from(COLS));
        let facts = PetWorldFacts::read(&self.term, 7);
        if self.ink {
            let mut world = PetWorld::default();
            assert!(world.observe(&input, &facts, PetPane::full(&input)));
            let spans: Vec<_> = (0..usize::from(ROWS))
                .map(|row| {
                    let first =
                        (0..usize::from(COLS)).find(|&col| world.ink_at(row, col) == Some(true));
                    let last =
                        (0..usize::from(COLS)).rfind(|&col| world.ink_at(row, col) == Some(true));
                    first
                        .zip(last)
                        .map_or((0, 0), |(first, last)| (first as u16, last as u16 + 1))
                })
                .collect();
            let live = spans
                .iter()
                .rposition(|&(first, end)| end > first)
                .map(|row| row as u16);
            self.brain.sense_ink(0, &spans, live);
        }
        self.brain
            .observe_console(&input, &facts, PetPane::full(&input));
        self.brain.set_console_presentable(self.presentable);
        self.brain.tick(PetSense {
            now,
            caret: (input.cursor_visible && input.display_offset == 0)
                .then_some((input.cursor_row as u16, input.cursor_col as u16)),
            wrapped: false,
            rows: ROWS,
            cols: COLS,
            cell_w: CELL_W,
            cell_h: CELL_H,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
        })
    }
}

fn body(frame: PetFrame) -> Option<PetRect> {
    frame
        .body_px(CELL_W, CELL_H, COLS, ROWS)
        .map(|(x0, x1, y0, y1)| {
            PetRect::new(
                y0 as f32 / f32::from(CELL_H),
                x0 as f32 / f32::from(CELL_W),
                (y1 - y0) as f32 / f32::from(CELL_H),
                (x1 - x0) as f32 / f32::from(CELL_W),
            )
        })
}

/// Where the command's output leaves the caret when it pauses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Park {
    /// After a trailing newline: column 0 of a fresh row, as `echo` leaves it.
    LineStart,
    /// Mid-row, at the end of the last printed line, as a progress line
    /// without a newline leaves it.
    LineEnd,
}

/// The settled body, measured against the caret it settled beside.
#[derive(Clone, Copy, Debug)]
struct Settled {
    /// `frame.col - caret_col`: the sprite's left edge past the caret.
    gap: f32,
    /// The same gap read off the emitted body rectangle.
    body_gap: Option<f32>,
    caret: (u16, u16),
    frame: PetFrame,
    /// Frames after output paused before the body came to rest.
    frames: u32,
    reason: &'static str,
    attention: PetAttention,
}

/// `pet_position_authority`'s streaming case — one 57-column line every
/// 240 ms into a screen that is not yet full, the newline landing on the
/// following frame as a real `echo` does — with a shell-integration block
/// left EXECUTING, so the console layer's output watcher is the arm that
/// answers. Then the output PAUSES with the caret parked, and the body is
/// left alone until it stops moving.
fn watch(presentable: bool, ink: bool, park: Park, trace: &mut Vec<String>) -> Settled {
    let mut now = Instant::now();
    let mut scene = Scene::new(
        presentable,
        ink,
        b"\x1b[1;1H\x1b]133;A\x07$ \x1b]133;B\x07build\r\n\x1b]133;C\x07",
    );
    let lines = 12u32;
    let mut line = 0u32;
    for i in 0..(lines * 15) {
        now += Duration::from_millis(16);
        if i % 15 == 0 {
            let text = format!("output line {line} ------------------------------------------");
            scene.term.process(text.as_bytes());
            line += 1;
        }
        if i % 15 == 1 && (park == Park::LineStart || line < lines) {
            scene.term.process(b"\r\n");
        }
        let f = scene.tick(now);
        trace.push(format!(
            "stream {i:3} col={:7.2} row={:6.2} caret={:?} alpha={} nf={} legacy={} handoff={} {} {:?}",
            f.col,
            f.row,
            (scene.term.cursor().row, scene.term.cursor().col),
            f.alpha,
            scene.brain.needs_frames(),
            scene.brain.console_legacy_motion_pending(),
            scene.brain.console_resident_handoff_pending(),
            scene.brain.console_reason(),
            scene.brain.console_attention(),
        ));
    }
    let cursor = scene.term.cursor();
    let caret = (cursor.row, cursor.col);
    let mut still = 0u32;
    let mut frame = scene.tick(now);
    let mut frames = 0u32;
    for i in 0..900u32 {
        now += Duration::from_millis(16);
        frame = scene.tick(now);
        frames = i + 1;
        trace.push(format!(
            "parked {i:3} col={:7.2} row={:6.2} caret={caret:?} alpha={} nf={} legacy={} handoff={} {} {:?}",
            frame.col,
            frame.row,
            frame.alpha,
            scene.brain.needs_frames(),
            scene.brain.console_legacy_motion_pending(),
            scene.brain.console_resident_handoff_pending(),
            scene.brain.console_reason(),
            scene.brain.console_attention()
        ));
        if scene.brain.needs_frames() {
            still = 0;
        } else {
            still += 1;
            if still >= 30 {
                break;
            }
        }
    }
    let c = f32::from(caret.1);
    Settled {
        gap: frame.col - c,
        body_gap: body(frame).map(|b| b.col - c),
        caret,
        frame,
        frames,
        reason: scene.brain.console_reason(),
        attention: scene.brain.console_attention(),
    }
}

/// The v0.76.0 station, computed from the shipped law rather than recalled.
fn escort_station(caret_col: u16) -> f32 {
    PetBrain::station(caret_col, COLS, art_cols(CELL_W, CELL_H))
}

/// Every cell of the standoff, laid out so a failure message reads as data.
fn report(tag: &str, s: &Settled) -> String {
    format!(
        "{tag}: gap={:.2} body_gap={:?} caret={:?} col={:.2} row={:.2} alpha={} frames={} \
         reason={} attention={:?} escort_station={:.2}",
        s.gap,
        s.body_gap,
        s.caret,
        s.frame.col,
        s.frame.row,
        s.frame.alpha,
        s.frames,
        s.reason,
        s.attention,
        escort_station(s.caret.1),
    )
}

/// THE PIN. For every way a command's output can leave the caret parked —
/// column 0 after a newline, mid-row after a progress line — with and
/// without the host's ink seam fed, the body the console layer's resident
/// settles is within one cell of the body the v0.76.0 escort settles, on
/// the same row. Measured before this pin: escort 1.30 cells past the
/// caret on the caret's row; resident 3.00–3.10 cells past it, one row
/// below (`caret + STATION_LEAD + HOME_BREATHING_ROOM`, then a strict
/// perch search that could not answer with anything inside the caret's
/// ring). After: resident 1.00–1.19, escort 1.30, same row.
#[test]
fn the_resident_settles_where_the_escort_settles() {
    let mut table = Vec::new();
    let mut failures = Vec::new();
    for ink in [false, true] {
        for park in [Park::LineStart, Park::LineEnd] {
            let mut bare_trace = Vec::new();
            let mut live_trace = Vec::new();
            let bare = watch(false, ink, park, &mut bare_trace);
            let live = watch(true, ink, park, &mut live_trace);
            let tag = format!("ink={ink} park={park:?}");
            table.push(report(&format!("escort   {tag}"), &bare));
            table.push(report(&format!("resident {tag}"), &live));
            // PRECONDITIONS, so the pin cannot pass vacuously: both bodies
            // are on glass, the caret parked where the fixture says, and the
            // live brain really took the resident path.
            assert!(
                bare.frame.alpha > 0 && live.frame.alpha > 0,
                "{tag}: a body is off glass"
            );
            assert_eq!(
                bare.caret, live.caret,
                "{tag}: the twins saw different carets"
            );
            assert_eq!(
                live.attention,
                PetAttention::Output,
                "{tag}: precondition — the live brain must be watching the command \
                 (reason {})",
                live.reason,
            );
            let dcol = (live.gap - bare.gap).abs();
            let drow = (live.frame.row - bare.frame.row).abs();
            if dcol > 1.0 || drow > 0.5 {
                failures.push(format!(
                    "{tag}: resident gap {:.2} vs escort gap {:.2} (dcol {dcol:.2}); rows \
                     {:.2} vs {:.2} (drow {drow:.2})",
                    live.gap, bare.gap, live.frame.row, bare.frame.row,
                ));
            }
            // AND THE RESIDENT IS ON THE ESCORT'S LADDER, not merely close to
            // wherever the escort happened to stop: `pet_escort_primacy`'s
            // own bound, half a cell of slack for the chase's easing and
            // `body_px`'s rounding; a whole extra cell of standoff is the
            // thing this test refuses.
            let want = escort_station(live.caret.1);
            if (live.frame.col - want).abs() > 0.5 {
                failures.push(format!(
                    "{tag}: resident stood at {:.2}; the v0.76 ladder says {want:.2} \
                     (caret col {})",
                    live.frame.col, live.caret.1,
                ));
            }
            if std::env::var_os("STANDOFF_TRACE").is_some() {
                for line in bare_trace
                    .iter()
                    .map(|l| format!("bare {l}"))
                    .chain(live_trace.iter().map(|l| format!("live {l}")))
                {
                    println!("    {line}");
                }
            }
        }
    }
    for line in &table {
        println!("{line}");
    }
    assert!(
        failures.is_empty(),
        "the resident and the escort settle in different places:\n  {}\n{}",
        failures.join("\n  "),
        table.join("\n"),
    );
}
