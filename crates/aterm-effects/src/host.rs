// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The host contract: the plain data every host hands the effects driver for one
//! frame, and the plain data it gets back
//! (`docs/DESIGN-host-boundary-2026-08-30.md` §3). Everything is `Copy` where
//! possible, names no platform type, and reads time only as
//! [`aterm_time::Instant`], so one driver serves the macOS host, the
//! Linux/Windows arms and the JS hosts (`aterm-wasm`, `aterm-gpu-web`).
//!
//! Two seams are not traits called inside the frame: the durable Kitty Log (the
//! favourite is an input; sightings go back to the host, which records them)
//! and entropy (the seed is a value the host mints: `aterm_uds::rand` natively,
//! `crypto.getRandomValues` on the page). The engine requests; the frontend
//! performs.

pub use aterm_time::Instant;

use aterm_core::terminal::{ShellState, Terminal};

/// The host's tri-state presentability (the pipeline's `set_effects_visibility`
/// as a type): `Focused` (full profile), `VisibleUnfocused` (calm cap and
/// drain), `Hidden` (a hard cursor-coordinate boundary: the resident pet
/// retires its coordinate space and returns as a fresh sighting).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Visibility {
    Focused,
    VisibleUnfocused,
    Hidden,
}

/// What kind of frame this is. A capture is one isolated frame: it never reads
/// or spends the frame-over-frame diffs a present owns (output burst, wrap
/// fact, completion latch, Execute level) and never sees a pointer, so it does
/// not steal the next present's baseline.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CaptureMode {
    /// A drawn present: the live tick, and the diffs are read and spent.
    Present,
    /// A capture on a surface that also presents (a windowed instance): the
    /// live tick, so the capture shows the pet where the glass has it.
    LiveCapture,
    /// A windowless still ([`crate::kitty_pet::PetBrain::tick_static_capture`]):
    /// never advances a walk, and the first still shows the resident at full
    /// opacity rather than the `dt == 0` fade-in tick.
    StaticCapture,
}

/// The frame's grid: cell metrics, extent, and where the grid's `(0, 0)` sits in
/// frame pixels (the effects origin, chrome `pad`/`pad + head`, plus on a
/// composed surface the focused pane's pixel origin).
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameGeom {
    pub rows: u16,
    pub cols: u16,
    pub cell_w: u16,
    pub cell_h: u16,
    /// Grid origin in FRAME px (effects origin, + pane origin when composed).
    pub origin_px: (i32, i32),
}

/// Everything a host tells the engine for ONE frame.
#[derive(Clone, Copy, Debug)]
pub struct HostFrameInput {
    /// Native: the one `frame_started` read; web: `t0 + Σ advance(dt)`.
    pub now: Instant,
    pub visibility: Visibility,
    /// Stable accessibility/focus policy: the ONLY input that pins the
    /// resident at its station. Never a load term.
    pub reduced_motion: bool,
    /// Serious mode: every toy is retired outright.
    pub serious: bool,
    /// The adaptive load-shed envelope `0..=1`. It attenuates trails and
    /// flying heads; the full resident ignores it.
    pub shed_envelope: f32,
    /// The load-shed latch itself is engaged. The resident keeps its motion
    /// and leaves the frame train for the coarse
    /// [`RESIDENT_SHED_CADENCE`](crate::companion::RESIDENT_SHED_CADENCE).
    /// A host with no shed passes `false`.
    pub shed_active: bool,
    /// The pointer in frame px; `None` means it left the surface, or (for a host
    /// pushing pointer events between frames) not fed with the frame. A `Some`
    /// here wins over the value stored through `CompanionOwner::set_pointer`
    /// (`CompanionOwner::sense`); `None` falls back to it. The native host passes
    /// `last_cursor_px` every frame; the web host feeds the setter from
    /// `note_pointer_px` and leaves this `None`.
    pub pointer_px: Option<(f32, f32)>,
    pub capture: CaptureMode,
    pub geometry: FrameGeom,
}

/// Facts read from the [`Terminal`] ONCE per frame under whatever lock the
/// host holds (the frame's one terminal hold natively; `&mut Terminal` on the
/// web). One reader for every host, so no two drivers can disagree about what
/// the emulator said.
///
/// Carries no text: typed witnesses enter only through each host's input path.
/// These are the facts a resident pet chases: where the caret is, whether the
/// emulator wrapped it, whether the pane is streaming, what just finished.
#[derive(Clone, Copy, Debug)]
pub struct TerminalFacts {
    /// The session the facts belong to. Every latch is keyed on it, so a tab or
    /// pane switch re-baselines silently: never a wrap, a burst or a finished
    /// command.
    pub session: u64,
    /// The caret cell `(row, col)`, or `None` when the viewport is scrolled into
    /// history (a coordinate-space boundary). A hidden caret (DECTCEM) is still
    /// a caret: progress bars, spinners, pagers, editors and LLM consoles hide
    /// the cursor, and the pet must keep chasing it. Whether it is painted is
    /// [`Self::cursor_visible`]. A host with a coherent `RenderInput` snapshot
    /// may substitute the snapshot's caret so cells and caret agree.
    pub caret: Option<(u16, u16)>,
    /// The emulator is painting the caret (DECTCEM shown).
    pub cursor_visible: bool,
    /// The program's own account that the frame on screen is unfinished
    /// ([`Terminal::sync_frame_unfinished`]), read under the same lock as the
    /// caret. A HIDDEN caret on such a frame is the program's write position,
    /// not its caret ([`motion_caret`]), so the pet reads the caret of the
    /// last frame the program finished instead
    /// ([`crate::companion::CompanionOwner::prepare`]). A host that hands the
    /// pet no hidden caret passes `false`.
    pub frame_unfinished: bool,
    pub display_offset: i32,
    /// `display_offset == 0`: the viewport is at the live bottom.
    pub live_viewport: bool,
    pub content_seq: u64,
    pub wrap_serial: u64,
    /// The host's content-scroll decision resolved to `Translate` this frame
    /// (new scrollback rows).
    pub scrolled: bool,
    /// OSC 133/633 `C..D`: a command is executing.
    pub shell_executing: bool,
    /// The most recent COMPLETED command: `(completed_command_seq, exit code,
    /// exec duration ms)`. `None` before the first completion.
    pub cmd_done: Option<(u64, i32, Option<u64>)>,
    pub alt_screen: bool,
}

impl TerminalFacts {
    /// Read every fact from the terminal in one pass. `scrolled` is the host's
    /// own content-scroll decision (it owns the previous snapshot); the rest is
    /// the emulator's.
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

/// The caret the motion engines may judge: [`TerminalFacts::caret`] minus a
/// hidden cursor that is not a caret.
///
/// A hidden caret still travels to the pet (it chases a caret a TUI hid for
/// its repaint — [`TerminalFacts::caret`]), and the single-pane window hands
/// one to the cursor-motion engines too. On an unfinished frame it is not
/// the caret, though, for either of them (the pet's side is
/// [`TerminalFacts::frame_unfinished`]). The program has said the frame is
/// unfinished, hidden the cursor, and is walking it through the screen as
/// its WRITE position.
/// A host shows such a frame only when it stops waiting for the close:
/// its own hold runs out (`SYNC_HOLD_CAP`, 150 ms, with the mode still set),
/// or the terminal's `sync_timeout_ms` (1 s) force-clears the mode while
/// the program is still writing. Either way the cursor is caught wherever
/// the partial write happened to stop. The mode level alone misses the
/// second case, which is why the input is the program's account.
///
/// A hidden cursor on a blank row is no caret either: Claude Code's question
/// dialog leaves its hidden cursor at column 0 of the blank bottom row on its
/// last option, where there is no glyph to arrive at. A shown cursor on a blank
/// row (a fresh prompt, an empty editor line) passes, as does a hidden one
/// beside a styled blank (a self-drawn caret). A shown caret passes on an
/// unfinished frame, and a hidden one on a finished frame (including a bracket
/// closed and reopened with nothing written). [`hidden_caret_withheld`] reads
/// both cases under the same lock as the caret.
#[must_use]
pub fn motion_caret(
    caret: Option<(u16, u16)>,
    cursor_visible: bool,
    hidden_withheld: bool,
) -> Option<(u16, u16)> {
    caret.filter(|_| cursor_visible || !hidden_withheld)
}

/// Whether a hidden caret on `term` is withheld from the motion engines
/// ([`motion_caret`]'s third input): the frame is unfinished
/// ([`Terminal::sync_frame_unfinished`]), or no cell of the cursor's row draws
/// anything (`draws_ink`). A styled blank (a TUI's inverse-space caret, a
/// coloured line) counts as something. Read under the same lock as the caret;
/// the row is walked only while the cursor is hidden.
#[must_use]
pub fn hidden_caret_withheld(term: &Terminal) -> bool {
    term.sync_frame_unfinished() || (!term.cursor_visible() && cursor_row_empty(term))
}

/// The echo anchor the motion engines may judge: [`Terminal::print_anchor`] and
/// the glyph its run ended on ([`Terminal::print_anchor_glyph`]), or neither
/// where [`motion_caret`] withholds the caret for the same reason: a hidden
/// cursor on a frame the program has not finished
/// ([`Terminal::sync_frame_unfinished`]). Read under the same lock as the
/// caret.
///
/// The anchor is the other half of a write position. On a frame the program
/// left half-written behind a hidden cursor, its last print run ended wherever
/// the write stopped, as the cursor did. Claude Code's echo reprints the text
/// after the caret, so a torn echo's run ends at the line's end, where no key
/// was typed. The hidden-caret lane would spend the key in flight there, and
/// the key's own cell would never be laid. Withheld, the engine keeps its
/// previous sample and the lane idles; the finished frame hands over the
/// anchor its write ended on, beside the caret it shows. A drawn caret is the
/// program's account of where it typed whatever the frame's state (an
/// unfinished latch outlives a program that died mid-frame), so its anchor
/// passes, as the caret does.
#[must_use]
pub fn motion_print_anchor(term: &Terminal) -> (Option<(u16, u16, u64)>, Option<char>) {
    if term.sync_frame_unfinished() && !term.cursor_visible() {
        (None, None)
    } else {
        (term.print_anchor(), term.print_anchor_glyph())
    }
}

fn cursor_row_empty(term: &Terminal) -> bool {
    let grid = term.grid();
    let row = term.cursor().row;
    (0..grid.cols()).all(|col| grid.cell(row, col).is_none_or(|cell| !draws_ink(cell)))
}

/// Whether `cell` puts anything on the glass: a glyph, a background, or a
/// decoration a space still shows (inverse, a line under, through or over it).
/// A foreground colour, bold, dim, italic or blink on a space draws nothing.
fn draws_ink(cell: &aterm_core::grid::Cell) -> bool {
    use aterm_core::grid::CellFlags;
    let flags = cell.flags();
    let shows_on_a_space = CellFlags::INVERSE
        .union(CellFlags::ALL_UNDERLINES)
        .union(CellFlags::STRIKETHROUGH);
    cell.char_data() != u16::from(b' ')
        || flags.contains(CellFlags::COMPLEX)
        || !cell.colors().bg_is_default()
        || flags.intersects(shows_on_a_space)
        // OVERLINE is SUPERSCRIPT | SUBSCRIPT: only both bits draw a line; a
        // super- or subscripted space alone draws nothing.
        || flags.contains(CellFlags::OVERLINE)
}

/// The sing-along coupling made explicit as an INPUT to the custody law
/// (which companion owns the frame). `KittySing` and `CursorCat` are the
/// pipeline's ([`crate::companions`]), but the two numbers are the host's
/// verdicts over them — the song's drive under its style gate and Serious
/// Mode, the head's alpha under its presentation gates — resolved on each
/// route that ticks the pet. The web passes zeros: no song, no flying head,
/// the pet owns every frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct SingFacts {
    /// The sing drive `0..=1` — the armed hold plus the whole wind-down.
    pub drive: f32,
    /// The flying head's presentation alpha as the host resolved it (already
    /// gated by its own owner and momentum), `0` when there is no head.
    pub flying_alpha: u8,
}

/// What a left press did. Crosses the wasm boundary as its `u8`; `Pass`
/// means the host keeps routing (selection, mouse reporting), anything else
/// means chrome won and the press is consumed.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PressOutcome {
    Pass = 0,
    Pet = 1,
}

/// One scheduling verdict: `Frames` == `is_active()` (a frame-cadence lane);
/// `At` == `next_deadline_ms()` (an exact engine wake); `Idle` == 0% idle.
/// The pipeline tests read the two predicates through it.
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

    /// The one reader, against a real emulator: a fresh terminal has a visible
    /// caret at the origin on a live viewport, no completed command;
    /// OSC 133 `C` flips `shell_executing`, and `D;<code>` lands as `cmd_done`
    /// with the monotonic completion seq.
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

    /// DECTCEM hides the caret without withdrawing it (`cursor_visible` carries
    /// the hide); scrolling into history withdraws it. The alternate screen
    /// reads as such.
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

    /// The motion caret is the facts' caret minus a hidden cursor on an
    /// unfinished frame (the program's write position), including after the
    /// terminal's timeout force-clears the mode mid-frame. A finished frame's
    /// hidden caret, a close+reopen with nothing written, and an unfinished
    /// frame's shown caret all pass.
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
        // Through the reader's own `frame_unfinished`, so the fact the pet
        // reads is pinned to the program's account too.
        let read = |term: &Terminal| {
            let f = TerminalFacts::read(term, 1, false);
            assert_eq!(f.frame_unfinished, term.sync_frame_unfinished());
            (
                f.caret,
                motion_caret(f.caret, f.cursor_visible, hidden_caret_withheld(term)),
            )
        };
        assert_eq!(read(&term), (Some((7, 4)), Some((7, 4))), "a shown caret");

        // Ink's repaint: open, hide, home, a transcript row part-written.
        term.process_at(b"\x1b[?2026h\x1b[?25l\x1b[H\r\x1b[2Btranscript", at(10));
        let (facts, motion) = read(&term);
        assert_eq!(facts, Some((2, 10)), "the pet still reads the cursor");
        assert_eq!(motion, None, "the write position is no caret");

        // Past the terminal's 1 s timeout the MODE is force-cleared while the
        // program is still writing its frame: still no caret.
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

        // Shown inside the program's bracket: a program that does not hide.
        term.process_at(b"\x1b[?25h", at(1_210));
        assert_eq!(read(&term), (Some((5, 4)), Some((5, 4))));

        // Hidden after the program closes: a finished frame's hidden caret
        // keeps the single-pane window's reading.
        term.process_at(b"\x1b[?25l\x1b[8;1Hprompt\x1b[8;5H\x1b[?2026l", at(1_220));
        assert_eq!(read(&term), (Some((7, 4)), Some((7, 4))));

        // Closed and reopened in one batch with nothing written since: the
        // grid is the finished frame, and its hidden caret passes.
        term.process_at(
            b"\x1b[9;1Hrow\x1b[?2026h\x1b[9;2H\x1b[?2026l\x1b[?2026h",
            at(1_230),
        );
        assert!(term.modes().synchronized_output());
        assert_eq!(read(&term), (Some((8, 1)), Some((8, 1))));

        // History still withdraws the caret from everyone.
        assert_eq!(motion_caret(None, true, false), None);
    }

    /// Claude Code's mid-line echo torn before its parking CUP: the print run
    /// ends at the reprinted tail's end, a write position, so no anchor is
    /// handed over. Controls: the finished echo's anchor passes, and so do the
    /// anchor of a frame closed and reopened with nothing written and the
    /// anchor beside a drawn caret on an unfinished frame (a program that died
    /// mid-frame leaves the latch set under the shell's prompt).
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

        // The Space's echo, cut before its park: the tail reprinted from the
        // landing column, the caret still hidden.
        term.process(b"\x1b[?2026h\x1b[?25l\x1b[H\r\x1b[6C\x1b[7B the tail.");
        assert!(term.sync_frame_unfinished());
        assert!(
            term.print_anchor().is_some(),
            "control: the run ended somewhere"
        );
        assert_eq!(motion_print_anchor(&term), (None, None), "a write position");

        // The rest lands: the frame is finished and its anchor passes.
        term.process(b"\x1b[10;1H\x1b[8;7H\x1b[?25h\x1b[?2026l");
        let after = motion_print_anchor(&term);
        assert_eq!(after, (term.print_anchor(), term.print_anchor_glyph()));
        assert!(
            after
                .0
                .is_some_and(|(_, _, seq)| finished.0.is_some_and(|(_, _, s0)| seq > s0))
        );

        // Reopened with nothing written: the grid is the finished frame.
        term.process(b"\x1b[?2026h");
        assert_eq!(motion_print_anchor(&term), after);

        // Written into with the caret drawn: the anchor passes beside it.
        term.process(b"\x1b[9;1H$ ls");
        assert!(term.sync_frame_unfinished() && term.cursor_visible());
        let drawn = motion_print_anchor(&term);
        assert_eq!(drawn, (term.print_anchor(), term.print_anchor_glyph()));
        assert!(drawn.0.is_some_and(|(row, _, _)| row == 8));
    }

    /// Claude Code's question dialog on its last option: the hidden cursor moves
    /// to column 0 of the blank bottom row and is no caret there. Controls: the
    /// same hidden cursor on the option's row passes, and a shown cursor on a
    /// blank row passes.
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

        // ↓ to `Chat about this`: parked on the blank bottom row.
        term.process(b"\x1b[12;1H");
        let (facts, motion) = read(&term);
        assert_eq!(facts, Some((11, 0)), "the pet still reads the cursor");
        assert_eq!(motion, None, "a hidden cursor on a blank row is no caret");

        // Shown there, it is the caret.
        term.process(b"\x1b[?25h");
        assert_eq!(read(&term), (Some((11, 0)), Some((11, 0))));

        // A blank row padded with invisibly styled spaces (dim, a foreground
        // colour) still draws nothing: withheld.
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

        // Hidden again beside its own inverse-space caret: the row's one mark
        // is styled, so the caret passes.
        term.process(b"\x1b[?25l\x1b[12;1H\x1b[7m \x1b[27m\x1b[12;1H");
        assert_eq!(read(&term), (Some((11, 0)), Some((11, 0))));
    }
}
