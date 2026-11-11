// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The host contract: the plain data every host hands the effects driver for one frame, and the plain data it gets back (`docs/DESIGN-host-boundary-2026-08-30.md` §3).

pub use aterm_time::Instant;

use aterm_core::terminal::{ShellState, Terminal};

pub mod light_latch;
mod trail_ground;

pub use trail_ground::{PageFacts, TrailGround, pane_light};

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
#[derive(Clone, Copy, Debug)]
pub struct FrameGeom {
    pub rows: u16,
    pub cols: u16,
    pub cell_w: u16,
    pub cell_h: u16,
    /// Grid origin in FRAME px (effects origin, + pane origin when composed).
    pub origin_px: (i32, i32),
}

impl FrameGeom {
    /// Zeroed frame geometry before any frame is prepared.
    pub const EMPTY: Self = Self {
        rows: 0,
        cols: 0,
        cell_w: 0,
        cell_h: 0,
        origin_px: (0, 0),
    };
}

impl Default for FrameGeom {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
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

impl HostFrameInput {
    /// A focused, full-motion, unshed live present at `now` over `geometry`, with the pointer off
    /// the surface; each `with_*` sets one field, and a field added later takes its default here,
    /// so no host or harness literal changes (§15.1 P0.1).
    #[inline]
    #[must_use]
    pub const fn new(now: Instant, geometry: FrameGeom) -> Self {
        Self {
            now,
            visibility: Visibility::Focused,
            reduced_motion: false,
            serious: false,
            shed_envelope: 1.0,
            shed_active: false,
            pointer_px: None,
            capture: CaptureMode::Present,
            geometry,
        }
    }

    #[inline]
    #[must_use]
    pub const fn with_visibility(mut self, visibility: Visibility) -> Self {
        self.visibility = visibility;
        self
    }

    #[inline]
    #[must_use]
    pub const fn with_reduced_motion(mut self, reduced_motion: bool) -> Self {
        self.reduced_motion = reduced_motion;
        self
    }

    #[inline]
    #[must_use]
    pub const fn with_serious(mut self, serious: bool) -> Self {
        self.serious = serious;
        self
    }

    #[inline]
    #[must_use]
    pub const fn with_shed_envelope(mut self, shed_envelope: f32) -> Self {
        self.shed_envelope = shed_envelope;
        self
    }

    #[inline]
    #[must_use]
    pub const fn with_shed_active(mut self, shed_active: bool) -> Self {
        self.shed_active = shed_active;
        self
    }

    #[inline]
    #[must_use]
    pub const fn with_pointer_px(mut self, pointer_px: Option<(f32, f32)>) -> Self {
        self.pointer_px = pointer_px;
        self
    }

    #[inline]
    #[must_use]
    pub const fn with_capture(mut self, capture: CaptureMode) -> Self {
        self.capture = capture;
        self
    }
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
        let caret = if live_viewport {
            let c = term.cursor();
            Some((c.row, c.col))
        } else {
            None
        };
        let cmd_seq = term.completed_command_seq();
        let cmd_done = if let Some(m) = term.last_completed_command()
            && let Some(code) = m.exit_code
        {
            Some((cmd_seq, code, m.exec_duration_ms()))
        } else {
            None
        };
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
    let cols = grid.cols();
    let mut col = 0;
    while col < cols {
        if let Some(cell) = grid.cell(row, col)
            && draws_ink(cell)
        {
            return false;
        }
        col += 1;
    }
    true
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
#[derive(Clone, Copy, Debug)]
pub struct SingFacts {
    /// The sing drive `0..=1` — the armed hold plus the whole wind-down.
    pub drive: f32,
    /// The flying head's presentation alpha as the host resolved it (already gated by its own owner and momentum), `0` when there is no head.
    pub flying_alpha: u8,
}

impl SingFacts {
    /// Inactive sing-along facts with zero drive and zero flying alpha.
    pub const EMPTY: Self = Self {
        drive: 0.0,
        flying_alpha: 0,
    };
}

impl Default for SingFacts {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
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
#[path = "host_tests.rs"]
mod tests;
