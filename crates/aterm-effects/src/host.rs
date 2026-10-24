// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The host contract: the plain data every host hands the effects driver for one frame, and the plain data it gets back (`docs/DESIGN-host-boundary-2026-08-30.md` §3).

pub use aterm_time::Instant;

use aterm_core::terminal::{ShellState, Terminal};

/// The host's tri-state presentability (the pipeline's `set_effects_visibility` as a type): `Focused` (full profile), `VisibleUnfocused` (calm cap and drain), `Hidden` (a hard cursor-coordinate boundary: the resident pet retires its coordinate space and returns as a fresh sighting).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Visibility {
    Focused,
    VisibleUnfocused,
    Hidden,
}

/// What kind of frame this is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CaptureMode {
    /// A drawn present: the live tick, and the diffs are read and spent.
    Present,
    /// A capture on a surface that also presents (a windowed instance): the live tick, so the capture shows the pet where the glass has it.
    LiveCapture,
    /// A windowless still ([`crate::kitty_pet::PetBrain::tick_static_capture`]): never advances a walk, and the first still shows the resident at full opacity rather than the `dt == 0` fade-in tick.
    StaticCapture,
}

/// The frame's grid: cell metrics, extent and origin.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameGeom {
    pub rows: u16,
    pub cols: u16,
    pub cell_w: u16,
    pub cell_h: u16,
    /// Grid origin in FRAME px (effects origin, + pane origin when composed).
    pub origin_px: (i32, i32),
}

/// Everything a host tells the engine for one frame.
#[derive(Clone, Copy, Debug)]
pub struct HostFrameInput {
    /// Native: the one `frame_started` read; web: `t0 + Σ advance(dt)`.
    pub now: Instant,
    pub visibility: Visibility,
    /// Stable accessibility/focus policy: the only input that pins the resident at its station.
    pub reduced_motion: bool,
    /// Serious mode: every toy is retired outright.
    pub serious: bool,
    /// The adaptive load-shed envelope `0..=1`.
    pub shed_envelope: f32,
    /// The load-shed latch is engaged: the resident keeps its motion but leaves the frame train for the coarse [`RESIDENT_SHED_CADENCE`](crate::companion::RESIDENT_SHED_CADENCE).
    pub shed_active: bool,
    /// The pointer in frame px, or `None` (it left the surface, or the host pushes pointer events between frames).
    pub pointer_px: Option<(f32, f32)>,
    pub capture: CaptureMode,
    pub geometry: FrameGeom,
}

/// Facts read from the [`Terminal`] once per frame under whatever lock the host holds, by one reader for every host.
#[derive(Clone, Copy, Debug)]
pub struct TerminalFacts {
    /// The session the facts belong to.
    pub session: u64,
    /// The caret cell `(row, col)`, or `None` when the viewport is scrolled into history (a coordinate-space boundary).
    pub caret: Option<(u16, u16)>,
    /// The emulator is painting the caret (DECTCEM shown).
    pub cursor_visible: bool,
    /// The program's own account that the frame on screen is unfinished ([`Terminal::sync_frame_unfinished`]), read under the caret's lock.
    pub frame_unfinished: bool,
    /// The terminal's count of synchronized-output closes ([`Terminal::sync_end_seq`]), read under the same lock: a program that brackets its frames moves it on with every frame it finishes.
    pub sync_end_seq: u64,
    /// The latest close's cell on this screen/grid, read under the same lock ([`Terminal::sync_end_caret`]); `None` after an invalidating boundary.
    pub sync_end_caret: Option<(u16, u16)>,
    pub display_offset: i32,
    /// `display_offset == 0`: the viewport is at the live bottom.
    pub live_viewport: bool,
    pub content_seq: u64,
    pub wrap_serial: u64,
    /// The host's content-scroll decision resolved to `Translate` this frame (new scrollback rows).
    pub scrolled: bool,
    /// OSC 133/633 `C..D`: a command is executing.
    pub shell_executing: bool,
    /// The most recent completed command: `(completed_command_seq, exit code, exec duration ms)`.
    pub cmd_done: Option<(u64, i32, Option<u64>)>,
    pub alt_screen: bool,
}

impl TerminalFacts {
    /// Read every fact from the terminal in one pass.
    #[inline]
    #[must_use]
    pub fn read(term: &Terminal, session: u64, scrolled: bool) -> Self {
        let display_offset = term.grid().display_offset() as i32;
        let live_viewport = display_offset == 0;
        let cursor_visible = term.cursor_visible();
        let caret = live_viewport.then(|| {
            let c = term.cursor();
            (c.row, c.col)
        });
        let cmd_seq = term.completed_command_seq();
        let cmd_done = term.last_completed_command().and_then(|m| {
            m.exit_code
                .map(|code| (cmd_seq, code, m.exec_duration_ms()))
        });
        Self {
            session,
            caret,
            cursor_visible,
            frame_unfinished: term.sync_frame_unfinished(),
            sync_end_seq: term.sync_end_seq(),
            sync_end_caret: term.sync_end_caret(),
            display_offset,
            live_viewport,
            content_seq: term.content_seq(),
            wrap_serial: term.wrap_serial(),
            scrolled,
            shell_executing: term.shell_state() == ShellState::Executing,
            cmd_done,
            alt_screen: term.is_alternate_screen(),
        }
    }
}

/// The caret the motion engines may judge: [`TerminalFacts::caret`] minus a hidden cursor that is not a caret.
#[inline]
#[must_use]
pub const fn motion_caret(
    caret: Option<(u16, u16)>,
    cursor_visible: bool,
    hidden_withheld: bool,
) -> Option<(u16, u16)> {
    if cursor_visible || !hidden_withheld {
        caret
    } else {
        None
    }
}

/// Whether a hidden caret on `term` is withheld from the motion engines ([`motion_caret`]'s third input): the frame is unfinished, or no cell of the cursor's row draws ink (a styled blank counts as ink).
#[inline]
#[must_use]
pub fn hidden_caret_withheld(term: &Terminal) -> bool {
    term.sync_frame_unfinished() || (!term.cursor_visible() && cursor_row_empty(term))
}

/// The echo anchor the motion engines may judge: [`Terminal::print_anchor`] and the glyph its run ended on ([`Terminal::print_anchor_glyph`]), or neither where [`motion_caret`] withholds the caret for the same reason: a hidden cursor on a frame the program has not finished ([`Terminal::sync_frame_unfinished`]).
#[inline]
#[must_use]
pub fn motion_print_anchor(term: &Terminal) -> (Option<(u16, u16, u64)>, Option<char>) {
    if term.sync_frame_unfinished() && !term.cursor_visible() {
        (None, None)
    } else {
        (term.print_anchor(), term.print_anchor_glyph())
    }
}

#[inline]
fn cursor_row_empty(term: &Terminal) -> bool {
    let grid = term.grid();
    let row = term.cursor().row;
    (0..grid.cols()).all(|col| grid.cell(row, col).is_none_or(|cell| !draws_ink(cell)))
}

/// Whether `cell` puts anything on the glass: a glyph, a background, or a
/// decoration a space still shows (inverse, a line under, through or over it).
#[inline]
fn draws_ink(cell: &aterm_core::grid::Cell) -> bool {
    use aterm_core::grid::CellFlags;
    const SHOWS_ON_A_SPACE: CellFlags = CellFlags::COMPLEX
        .union(CellFlags::INVERSE)
        .union(CellFlags::ALL_UNDERLINES)
        .union(CellFlags::STRIKETHROUGH);
    let flags = cell.flags();
    cell.char_data() != u16::from(b' ')
        || !cell.colors().bg_is_default()
        || flags.intersects(SHOWS_ON_A_SPACE)
        || flags.contains(CellFlags::OVERLINE)
}

/// The sing-along input to the custody law (which companion owns the frame): the host's verdicts over the pipeline's `KittySing` and `CursorCat`, resolved on each route that ticks the pet.
#[derive(Clone, Copy, Debug, Default)]
pub struct SingFacts {
    /// The sing drive `0..=1` — the armed hold plus the whole wind-down.
    pub drive: f32,
    /// The flying head's presentation alpha as the host resolved it (already gated by its own owner and momentum), `0` when there is no head.
    pub flying_alpha: u8,
}

/// What a left press did.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PressOutcome {
    Pass = 0,
    Pet = 1,
}

/// One scheduling verdict: `At` from `next_deadline_ms()`, else `Frames` while `is_active()`, else `Idle`.
#[derive(Clone, Copy, Debug)]
#[cfg(test)]
pub enum Wake {
    Frames,
    At(Instant),
    Idle,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reader against a real emulator: fresh state, OSC 133 `C` as `shell_executing`,
    /// `D;<code>` as `cmd_done` with the completion seq.
    #[test]
    fn the_facts_reader_reports_what_the_emulator_said() {
        let mut term = Terminal::new(5, 20);
        let fresh = TerminalFacts::read(&term, 7, false);
        assert_eq!(fresh.session, 7);
        assert_eq!(fresh.caret, Some((0, 0)));
        assert!(fresh.cursor_visible && fresh.live_viewport);
        assert_eq!(fresh.display_offset, 0);
        assert!(!fresh.shell_executing && !fresh.alt_screen && !fresh.scrolled);
        assert_eq!(fresh.cmd_done, None);

        term.process(b"\x1b]133;A\x07\x1b]133;B\x07ls\r\n\x1b]133;C\x07");
        let executing = TerminalFacts::read(&term, 7, true);
        assert!(executing.shell_executing, "OSC 133 C is the Execute phase");
        assert!(
            executing.scrolled,
            "the host's scroll decision rides through"
        );
        assert!(
            executing.content_seq > fresh.content_seq,
            "output moved the content clock"
        );

        term.process(b"out\r\n\x1b]133;D;3\x07");
        let done = TerminalFacts::read(&term, 7, false);
        assert!(!done.shell_executing);
        let (seq, code, _dur) = done.cmd_done.expect("a completed command");
        assert_eq!(code, 3, "the exit code the shell reported");
        assert_eq!(seq, term.completed_command_seq());
        assert!(seq >= 1, "the completion seq counts once per D");
    }

    /// DECTCEM hides the caret without withdrawing it; scrolling into history withdraws it.
    #[test]
    fn a_hidden_cursor_keeps_its_caret_and_history_withdraws_it() {
        let mut term = Terminal::new(5, 20);
        term.process(b"\x1b[3;7H\x1b[?25l");
        let hidden = TerminalFacts::read(&term, 1, false);
        assert_eq!(hidden.caret, Some((2, 6)), "a hidden caret still travels");
        assert!(!hidden.cursor_visible, "…and is reported unpainted");
        assert!(hidden.live_viewport, "hidden is not scrolled");

        for _ in 0..12 {
            term.process(b"line\r\n");
        }
        term.scroll_display(2);
        let history = TerminalFacts::read(&term, 1, false);
        assert!(!history.live_viewport);
        assert_eq!(history.caret, None, "history withdraws the caret");

        term.scroll_display(-2);
        term.process(b"\x1b[?25h\x1b[?1049h");
        let alt = TerminalFacts::read(&term, 1, false);
        assert!(alt.alt_screen);
        assert!(alt.caret.is_some() && alt.cursor_visible);
    }

    /// A hidden cursor on an unfinished frame is no motion caret, also after the terminal's
    /// timeout force-clears the mode.
    #[test]
    fn a_hidden_cursor_on_an_unfinished_frame_is_no_caret_for_the_motion_engines() {
        use aterm_core::terminal::ClockReading;
        let mut term = Terminal::new(10, 40);
        let t0 = std::time::Instant::now();
        let at = |ms: u64| ClockReading {
            monotonic: t0 + std::time::Duration::from_millis(ms),
            wall_ms: None,
        };
        term.process_at(b"\x1b[8;5H", at(0));
        let read = |term: &Terminal| {
            let f = TerminalFacts::read(term, 1, false);
            assert_eq!(f.frame_unfinished, term.sync_frame_unfinished());
            (
                f.caret,
                motion_caret(f.caret, f.cursor_visible, hidden_caret_withheld(term)),
            )
        };
        assert_eq!(read(&term), (Some((7, 4)), Some((7, 4))), "a shown caret");

        term.process_at(b"\x1b[?2026h\x1b[?25l\x1b[H\r\x1b[2Btranscript", at(10));
        let (facts, motion) = read(&term);
        assert_eq!(facts, Some((2, 10)), "the pet still reads the cursor");
        assert_eq!(motion, None, "the write position is no caret");

        term.process_at(b"\r\x1b[3Bmore", at(1_200));
        assert!(
            !term.modes().synchronized_output(),
            "control: the mode level reads closed"
        );
        let (facts, motion) = read(&term);
        assert_eq!(facts, Some((5, 4)), "the pet still reads the cursor");
        assert_eq!(
            motion, None,
            "a timed-out frame's write position is no caret"
        );

        term.process_at(b"\x1b[?25h", at(1_210));
        assert_eq!(read(&term), (Some((5, 4)), Some((5, 4))));

        term.process_at(b"\x1b[?25l\x1b[8;1Hprompt\x1b[8;5H\x1b[?2026l", at(1_220));
        assert_eq!(read(&term), (Some((7, 4)), Some((7, 4))));

        term.process_at(
            b"\x1b[9;1Hrow\x1b[?2026h\x1b[9;2H\x1b[?2026l\x1b[?2026h",
            at(1_230),
        );
        assert!(term.modes().synchronized_output());
        assert_eq!(read(&term), (Some((8, 1)), Some((8, 1))));

        assert_eq!(motion_caret(None, true, false), None);
    }

    /// Claude Code's mid-line echo torn before its parking CUP: the print run
    /// ends at the reprinted tail's end, a write position, so no anchor is
    /// handed over.
    #[test]
    fn an_unfinished_frames_print_anchor_is_no_echo_for_the_motion_engines() {
        let mut term = Terminal::new(10, 40);
        term.process(b"\x1b[8;1H> the tail.\x1b[8;6H");
        let finished = motion_print_anchor(&term);
        assert!(
            finished.0.is_some(),
            "a finished frame hands its anchor over"
        );
        assert_eq!(finished, (term.print_anchor(), term.print_anchor_glyph()));

        term.process(b"\x1b[?2026h\x1b[?25l\x1b[H\r\x1b[6C\x1b[7B the tail.");
        assert!(term.sync_frame_unfinished());
        assert!(
            term.print_anchor().is_some(),
            "control: the run ended somewhere"
        );
        assert_eq!(motion_print_anchor(&term), (None, None), "a write position");

        term.process(b"\x1b[10;1H\x1b[8;7H\x1b[?25h\x1b[?2026l");
        let after = motion_print_anchor(&term);
        assert_eq!(after, (term.print_anchor(), term.print_anchor_glyph()));
        assert!(
            after
                .0
                .is_some_and(|(_, _, seq)| finished.0.is_some_and(|(_, _, s0)| seq > s0))
        );

        term.process(b"\x1b[?2026h");
        assert_eq!(motion_print_anchor(&term), after);

        term.process(b"\x1b[9;1H$ ls");
        assert!(term.sync_frame_unfinished() && term.cursor_visible());
        let drawn = motion_print_anchor(&term);
        assert_eq!(drawn, (term.print_anchor(), term.print_anchor_glyph()));
        assert!(drawn.0.is_some_and(|(row, _, _)| row == 8));
    }

    /// Claude Code's question dialog parks its hidden cursor at column 0 of the blank bottom row:
    /// no caret there.
    #[test]
    fn a_hidden_cursor_on_a_blank_row_is_no_caret_for_the_motion_engines() {
        let mut term = Terminal::new(12, 40);
        term.process(b"\x1b[?25l\x1b[5;1H\xe2\x80\xba 3. Type something.");
        term.process(b"\x1b[7;1H  4. Chat about this\x1b[5;1H");
        let read = |term: &Terminal| {
            let f = TerminalFacts::read(term, 1, false);
            (
                f.caret,
                motion_caret(f.caret, f.cursor_visible, hidden_caret_withheld(term)),
            )
        };
        assert_eq!(
            read(&term),
            (Some((4, 0)), Some((4, 0))),
            "the option's row"
        );

        term.process(b"\x1b[12;1H");
        let (facts, motion) = read(&term);
        assert_eq!(facts, Some((11, 0)), "the pet still reads the cursor");
        assert_eq!(motion, None, "a hidden cursor on a blank row is no caret");

        term.process(b"\x1b[?25h");
        assert_eq!(read(&term), (Some((11, 0)), Some((11, 0))));

        term.process(b"\x1b[?25l\x1b[12;1H\x1b[2m    \x1b[38;5;244m    \x1b[0m\x1b[12;1H");
        assert_eq!(read(&term).1, None, "invisible styling is no mark");
        term.process(b"\x1b[12;1H\x1b[73m    \x1b[74m    \x1b[0m\x1b[12;1H");
        assert_eq!(
            read(&term).1,
            None,
            "a super- or subscripted space is no overline"
        );
        term.process(b"\x1b[12;1H\x1b[53m \x1b[0m\x1b[12;1H");
        assert_eq!(
            read(&term),
            (Some((11, 0)), Some((11, 0))),
            "an overline is a mark"
        );
        term.process(b"\x1b[12;1H\x1b[2K");

        term.process(b"\x1b[?25l\x1b[12;1H\x1b[7m \x1b[27m\x1b[12;1H");
        assert_eq!(read(&term), (Some((11, 0)), Some((11, 0))));
    }
}
