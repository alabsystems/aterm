// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HOST CONTRACT — the plain data every host hands the effects driver for
//! one frame, and the plain data it gets back.
//!
//! Owner, 2026-08-30: *"take a step back. Redesign this. The GUI should be
//! lightweight and specific to OSX, not having core logic like pets"* — and —
//! *"first, you need to migrate the pet from the app and move it into the
//! engine"*. This module is the boundary that direction draws
//! (`docs/DESIGN-host-boundary-2026-08-30.md` §3): everything in it is `Copy`
//! where possible, names no platform type, and reads time only as
//! [`aterm_time::Instant`] — so the same driver serves the macOS host, the
//! Linux/Windows arms and the JS hosts (`aterm-wasm`, `aterm-gpu-web`).
//!
//! Two seams are deliberately NOT traits called from inside the frame: the
//! durable Kitty Log (the favourite is an INPUT, sightings are EMITTED as
//! [`FrameEvent::Sighting`] and the host records them) and entropy (the seed
//! is a VALUE the host mints — `aterm_uds::rand` natively,
//! `crypto.getRandomValues` on the page). The engine REQUESTS, the frontend
//! PERFORMS.
//!
//! Phase 1 carries the resident pet. The Robi geometry on [`ChromeGeom`], the
//! event roster and [`Provenance`] are declared now so Phase 5 grows the
//! contract without renaming it.

pub use aterm_time::Instant;

use aterm_core::terminal::{BlockState, ShellState, Terminal};

use crate::companion::CompanionRung;
use crate::kitty_registry::KittyLook;

/// The host's tri-state presentability — the pipeline's `set_effects_visibility`
/// as a type: `Focused` (full profile), `VisibleUnfocused` (calm cap + drain),
/// `Hidden` (a hard cursor-coordinate boundary: the resident pet retires its
/// coordinate space on that edge and returns as a fresh sighting).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Visibility {
    Focused,
    VisibleUnfocused,
    Hidden,
}

/// THE PROVENANCE LAW: only a TYPED witness may arm typing-reactive effects;
/// `send`/paste are inert (`docs/INTROSPECTION.md`; the native app's law,
/// restated at the web binding as *"movement without one is program output
/// and stays dark"*). Declared here so Phase 5's `note_typed_edit` cannot be
/// added without naming which side of the law a call sits on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Provenance {
    Typed,
    Injected,
}

/// `Present` ticks the live brain; `StaticCapture` selects
/// [`crate::kitty_pet::PetBrain::tick_static_capture`] and the converged-capture
/// discipline — a capture never advances a walk, and the first windowless
/// still materialises the resident at full opacity instead of catching the
/// `dt == 0` first fade-in tick.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CaptureMode {
    Present,
    StaticCapture,
}

/// The frame's grid: cell metrics, extent, and where the grid's `(0, 0)` sits
/// in FRAME pixels — the effects origin (chrome `pad`/`pad + head`) plus, on a
/// composed surface, the focused pane's own pixel origin.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameGeom {
    pub rows: u16,
    pub cols: u16,
    pub cell_w: u16,
    pub cell_h: u16,
    /// Grid origin in FRAME px (effects origin, + pane origin when composed).
    pub origin_px: (i32, i32),
}

/// Chrome the effects need: `pad`/`head` (the pipeline's `set_chrome` law —
/// `[head][pad][grid][pad]`), and — Phase 5 — the Robi geometry
/// ([`crate::robi::RobiSense`]'s bar line, handholds and window clamp). Zero
/// handholds = Robi off, which is what every web host passes.
#[derive(Clone, Copy, Debug, Default)]
pub struct ChromeGeom {
    pub pad: u16,
    pub head: u16,
    pub strip_px: u16,
    pub win_top: i32,
    pub win_bot: i32,
    pub bar_y: i32,
    pub handholds: [i32; crate::robi::MAX_HANDHOLDS],
    pub handhold_count: u8,
}

/// Everything a host tells the engine for ONE frame.
#[derive(Clone, Copy, Debug)]
pub struct HostFrameInput {
    /// Native: the one `frame_started` read; web: `t0 + Σ advance(dt)`.
    pub now: Instant,
    pub visibility: Visibility,
    /// THE MOTION POLICY ONLY — the STABLE preference × focus (the native
    /// `MotionPolicy::resolve`), never the load-shed latch. `PetBrain`'s
    /// reduced-motion arm is a hard station PIN; raising it on a visible,
    /// walking cat welds the body to the caret and teleports it on the next
    /// keystroke. A shed is a request to spend less time DRAWING (that is
    /// [`Self::shed_envelope`]), not to change what the animal is.
    pub reduced_motion: bool,
    /// The serious-mode master: the glass belongs to the work, every toy is
    /// retired outright.
    pub serious: bool,
    /// The adaptive load-shed envelope `0..=1`, applied POST-tick to
    /// presentation alphas only. The companion brains keep their unscaled
    /// state; only the copied frame handed to the renderer is attenuated.
    pub shed_envelope: f32,
    pub chrome: ChromeGeom,
    /// The pointer in FRAME px; `None` = it left the surface — or, for a host
    /// that pushes pointer events BETWEEN frames instead, "not fed with the
    /// frame". THE PRECEDENCE (`CompanionOwner::sense`): a `Some` here wins
    /// over the value stored through `CompanionOwner::set_pointer`; `None`
    /// falls back to the stored one. The native host tracks `last_cursor_px`
    /// per window and hands it in here every frame; the web host feeds the
    /// setter from `note_pointer_px` and leaves this `None`.
    pub pointer_px: Option<(f32, f32)>,
    pub capture: CaptureMode,
    /// The host has a live sink AND its focus/master policy allows sound.
    pub sound_allowed: bool,
    pub geometry: FrameGeom,
}

/// The shell block's identity key — `(id, state, commandline present)`, the
/// exact key the native app-kitty slot re-derives a program claim on, so the
/// Phase 5 look policy can cache per pipeline instead of per session.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BlockIdentity {
    pub id: u64,
    pub state: BlockState,
    /// An OSC 633;E commandline is attached.
    pub commandline: bool,
}

/// Facts read from the [`Terminal`] ONCE per frame under whatever lock the
/// host holds (LOCK A natively; `&mut Terminal` on the web). One reader for
/// every host, so no two drivers can disagree about what the emulator said.
///
/// Carries NO text: typed witnesses enter only through each host's real
/// input path (the provenance law), and the facts here are the ones a
/// resident pet chases — where the caret is, whether the emulator wrapped
/// it, whether the pane is streaming, what just finished.
#[derive(Clone, Copy, Debug)]
pub struct TerminalFacts {
    /// The session the facts belong to. Every latch the driver keeps is keyed
    /// on it, so a tab or pane switch re-baselines SILENTLY: a session change
    /// is never a wrap, never a burst, never a finished command.
    pub session: u64,
    /// The visible caret cell `(row, col)`, or `None` when the cursor is
    /// hidden (DECTCEM) or the viewport is scrolled into history. A host
    /// holding a coherent `RenderInput` snapshot may substitute the
    /// snapshot's caret so the cell plane and the caret agree.
    pub caret: Option<(u16, u16)>,
    pub cursor_visible: bool,
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
    pub block: Option<BlockIdentity>,
    pub alt_screen: bool,
}

impl TerminalFacts {
    /// Read every fact from the terminal in one pass. `scrolled` is the
    /// host's own content-scroll decision (it owns the previous snapshot);
    /// everything else is the emulator's word.
    #[must_use]
    pub fn read(term: &Terminal, session: u64, scrolled: bool) -> Self {
        let display_offset = term.grid().display_offset() as i32;
        let live_viewport = display_offset == 0;
        let cursor_visible = term.cursor_visible();
        let caret = (live_viewport && cursor_visible).then(|| {
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
            display_offset,
            live_viewport,
            content_seq: term.content_seq(),
            wrap_serial: term.wrap_serial(),
            scrolled,
            shell_executing: term.shell_state() == ShellState::Executing,
            cmd_done,
            block: term.current_block().map(|b| BlockIdentity {
                id: b.id,
                state: b.state,
                commandline: b.commandline.is_some(),
            }),
            alt_screen: term.is_alternate_screen(),
        }
    }
}

/// The sing-along coupling made explicit as an INPUT, so the custody law
/// (which companion owns the frame) is evaluated engine-side even while
/// `CursorCat`/`KittySing` stay in the GUI (until Phase 5; then internal).
/// The web passes zeros: no song, no flying head, the pet owns every frame.
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
    Robi = 2,
    RobiBubble = 3,
}

/// This frame's clickable bodies in FRAME px, `(x0, x1, y0, y1)` right/bottom
/// exclusive — the pet's from its live drawn body offset by the frame origin.
/// `None` = nothing drawn, which is what clears a stale hit target.
#[derive(Clone, Copy, Debug, Default)]
pub struct HitRects {
    pub pet: Option<(i32, i32, i32, i32)>,
    pub robi: Option<(i32, i32, i32, i32)>,
    pub robi_bubble: Option<(i32, i32, i32, i32)>,
}

/// One scheduling verdict for the winit deadline fold and the wasm WF-1 gate.
/// `Frames` == `is_active()` (a frame-cadence lane); `At` == `next_deadline_ms()`
/// (an exact engine wake); `Idle` == 0% idle.
#[derive(Clone, Copy, Debug)]
pub enum Wake {
    Frames,
    At(Instant),
    Idle,
}

/// Where a kitty sighting came from — the Kitty Log's two provenances. Typed
/// sightings may present a discovery; ambient ones (grid-scanned OUTPUT
/// text — `cat` in a man page) count and collect only.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SightingSource {
    Typed,
    Ambient,
}

/// What the engine EMITS for the host's ledgers and chrome. The engine only
/// emits; the host records (Kitty Log), draws (the Robi tip bubble is chrome)
/// or persists (a Robi dismissal is a settings write).
#[derive(Clone, Copy, Debug)]
pub enum FrameEvent {
    CompanionArrived {
        look: KittyLook,
        rung: CompanionRung,
    },
    PetPetted,
    DogSummoned,
    RobiTip {
        index: u16,
    },
    RobiDismissed,
    Sighting {
        look: KittyLook,
        source: SightingSource,
    },
}

/// What one frame hands back: the overlay fingerprint the host folds into
/// its repaint key, the scheduling verdict, and the clickable bodies.
#[derive(Clone, Copy, Debug)]
pub struct HostFrameOutput {
    pub fp: u64,
    pub wake: Wake,
    pub hit: HitRects,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one reader, against a real emulator: a fresh terminal has a visible
    /// caret at the origin on a live viewport, no completed command, no block;
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
        assert_eq!(fresh.block, None);

        term.process(b"\x1b]133;A\x07\x1b]133;B\x07ls\r\n\x1b]133;C\x07");
        let executing = TerminalFacts::read(&term, 7, true);
        assert!(executing.shell_executing, "OSC 133 C is the Execute phase");
        assert!(
            executing.scrolled,
            "the host's scroll decision rides through"
        );
        assert!(
            executing
                .block
                .is_some_and(|b| b.state == BlockState::Executing),
            "the current block is the executing one"
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

    /// DECTCEM withholds the caret while every other fact keeps reporting; the
    /// alternate screen reads as such.
    #[test]
    fn a_hidden_cursor_withholds_the_caret_and_alt_screen_reads_true() {
        let mut term = Terminal::new(5, 20);
        term.process(b"\x1b[?25l");
        let hidden = TerminalFacts::read(&term, 1, false);
        assert_eq!(hidden.caret, None);
        assert!(!hidden.cursor_visible);
        assert!(hidden.live_viewport, "hidden is not scrolled");

        term.process(b"\x1b[?25h\x1b[?1049h");
        let alt = TerminalFacts::read(&term, 1, false);
        assert!(alt.alt_screen);
        assert!(alt.caret.is_some(), "the caret returns with DECTCEM");
    }

    /// The pointer-free defaults and the `u8` face of a press outcome — what
    /// the wasm export hands JS.
    #[test]
    fn press_outcomes_cross_the_boundary_as_their_u8() {
        assert_eq!(PressOutcome::Pass as u8, 0);
        assert_eq!(PressOutcome::Pet as u8, 1);
        assert_eq!(PressOutcome::Robi as u8, 2);
        assert_eq!(PressOutcome::RobiBubble as u8, 3);
        let chrome = ChromeGeom::default();
        assert_eq!(chrome.handhold_count, 0, "zero handholds = Robi off");
        assert!(HitRects::default().pet.is_none());
    }
}
