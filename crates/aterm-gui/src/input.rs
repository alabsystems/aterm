// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Phase 0.5 — the engine-neutral [`InputEvent`] vocabulary and the [`Source`]
//! audit tag for the [`App::input`](crate::App::input) convergence seam
//! (design Addendum A.2, `docs/design/HIERARCHICAL_SESSIONS.md`).
//!
//! TODAY human input (the winit `on_key`/`on_mouse_*`/`on_cursor_moved`/
//! `on_resize`/`on_focus` handlers) and controller input (the control verbs
//! `cmd_key`/`cmd_ctrl`/`cmd_mouse`/`cmd_scroll`/`cmd_paste`/`cmd_resize`/...)
//! flow through TWO parallel code paths that can drift: only the human path
//! reads real modifiers, runs the click-count FSM, emits intermediate motion
//! reports, snaps-to-bottom + clears-selection on a keystroke, carries the
//! Kitty base-layout key, reports focus, and resets the cursor blink. The
//! controller path hard-codes `mods=0`, has no click-count, jumps straight to a
//! selection result, drops events when tracking is off, and never snaps/clears/
//! resets-blink.
//!
//! This module is the FRONTEND-ONLY data layer: an `InputEvent` is plain data
//! plus engine *types* (`Key`, `Modifiers`, `MouseButton`, `SelectionSide`,
//! `SelectionType`) — no fs, no socket, no winit. Both sources BUILD an
//! `InputEvent` and feed it to the ONE policy site `App::input(ev, src)`, which
//! is the sole reader of `keyboard_mode()`/`mouse_tracking_enabled()` and the
//! sole caller of the encoders / `scroll_display` / `reset_blink` /
//! `apply_term_resize`, and of the press-path viewport snap + selection clear
//! (`app_input::apply_press_custody`, inlined into the seam's one term-lock
//! scope).
//!
//! Two corrections to what this paragraph used to claim. There is no
//! `fn clear_selection` anywhere in the workspace — the clear is
//! `text_selection_mut().clear()` under the seam's own lock. And `snap_to_bottom`
//! is NOT seam-exclusive: after SELECTION CUSTODY its callers outside the seam
//! are exactly the PASTE-or-ECHO arms, which never reach the seam and so must
//! snap for themselves —
//!
//!   * `app_input::input_paste` and `dispatch_action`'s `Action::Paste`
//!   * the hardcoded ⌘-V arm of `on_key`
//!   * the IME-composition arm of `on_key` (a composing key IS typing; the
//!     preedit paints at the cursor, so a scrolled-back composer would type
//!     off-screen)
//!
//! …plus `app_mouse::select_all`. Every OTHER caller was deleted: a press that
//! writes no bytes to the PTY expresses no typing intent and may not take the
//! user's reading position (SELECTION CUSTODY R1). (The predictive-echo
//! gate additionally reads the NARROW
//! `Terminal::kitty_suppresses_predictive_echo()` projection of the mode — a
//! read-only DISPLAY gate deciding whether a local guess may paint; it never
//! feeds an encoder, so the seam remains the sole byte-producing mode reader.)
//! The seam ends at the
//! existing 0e `SinkWriter` (`sink.write_frame_nonparking` — same byte order and
//! whole-frame atomicity, but the UI thread never parks on a wedged foreground's
//! full tty input buffer).
//!
//! `Source` is AUDIT-ONLY: the seam MUST NEVER branch behaviour on it (the
//! indistinguishability invariant). The byte-producing core [`seam_egress`]
//! takes NO `Source` — it is STRUCTURALLY impossible for it to branch — and the
//! gesture-state arms of `App::input` read ONLY data carried on the event (never
//! `self.mods`). The Tier-1 tests prove convergence two ways: the two REAL
//! builders (`build_key_input`/`cmd_*` parse) produce structurally-EQUAL events
//! for the same intent, and those events produce byte-identical sink output.

use std::sync::Mutex;

use aterm_core::selection::SelectionSide;
use aterm_core::terminal::Terminal;
use aterm_session::Op;
#[cfg(test)]
use aterm_session::sink::ImmediateWrite;
use aterm_session::sink::{InputEpoch, SinkWriter};
use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey, encode_key_with_event};
use aterm_types::mouse::MouseButton;

use crate::term_lock;

/// One logical input event, engine-neutral. Built identically by a winit handler
/// (`Source::Human`) and by a control verb (`Source::Controller`); the seam turns
/// it into PTY bytes / viewport side-effects the SAME way for both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputEvent {
    /// A key chord. `base_layout` is the US-QWERTY equivalent of the physical key,
    /// fed to `encode_key_with_layout` so Kitty `REPORT_ALTERNATE_KEYS` carries the
    /// 3rd CSI-u field identically for both sources (kills divergences f/h).
    Key {
        key: Key,
        mods: Modifiers,
        base_layout: Option<char>,
        /// Press / Repeat / Release. The human winit path only ever builds
        /// `Press` (releases are dropped at `on_key`); the `key` verb can request
        /// any via `type=press|repeat|release`, so a controller can drive the
        /// Kitty keyboard protocol's event-type CSI-u sub-field (`:1`/`:2`/`:3`)
        /// that a real key-up/repeat produces. Encoded ONCE by `seam_egress`.
        event_type: aterm_types::keyboard::KeyEventType,
    },
    /// Literal text to type: the `on_key` bare-`ev.text` fallback and the IME
    /// `Ime::Commit` path. Each char is encoded as a `Character` key under the
    /// current keyboard mode.
    Text(String),
    /// Raw bytes from an aterm `[key_sequences]` INPUT-POLICY rule: written to the
    /// PTY VERBATIM (never re-encoded), so a chord the user mapped sends exactly the
    /// bytes they chose regardless of the keyboard mode. Built only by the human
    /// `on_key` path today.
    KeySequence(Vec<u8>),
    /// A mouse button press/release at a grid cell. `mods` is the real modifier
    /// mask (kills a), `click_count` is the authoritative 1..=3 multi-click depth
    /// (kills b), `side` is the cell-half for selection boundaries (kills i), and
    /// `block` is the selection-TYPE intent for a single-click press (kills the
    /// ambient-state read: a controller can drive block-select, a human's held
    /// Alt is captured at build time and never leaks past the event).
    MouseButton {
        button: MouseButton,
        pressed: bool,
        row: u16,
        col: u16,
        mods: u8,
        click_count: u8,
        side: SelectionSide,
        /// Single-click press starts a `Block` (rectangular) selection rather
        /// than `Simple`. Human: `self.mods.alt_key()` snapshotted at build time.
        /// Controller: the `block=…` token (default `false`). Read ONLY here, as
        /// DATA — the seam never re-reads ambient modifier state for the type.
        block: bool,
        /// When `true`, a completed drag-select settled by THIS event's release
        /// must NOT auto-copy to the system clipboard / X11 PRIMARY — the
        /// copy-on-select SIDE-EFFECT is suppressed for this gesture. Set by the
        /// `mouse` control verb ONLY for a NON-OWNER (scoped-edge) gesture: a
        /// scoped `WriteInput` edge may pan and select (viewport nav is reading)
        /// but must not exfiltrate on-screen text through copy-on-select. A real
        /// human gesture and an Owner-scoped controller gesture both leave it
        /// `false`, so their copy-on-select is unaffected. It is a POLICY flag
        /// carried on the event (the same shape as `block`), NOT a `Source`
        /// branch — and it is a clipboard SIDE-EFFECT, never a PTY byte:
        /// `seam_egress` ignores it entirely, so the `bytes_human_eq_controller`
        /// byte-equality invariant is untouched.
        suppress_copy_on_select: bool,
        /// Sub-cell pixel offset INSIDE the (`row`,`col`) cell, for DEC 1016
        /// SGR-pixel mouse mode (the genuine winit cursor x/y minus the cell
        /// origin). The seam combines it with the cell origin + the engine's
        /// cell pixel size to emit a true pixel coordinate ONLY when 1016 is
        /// active; the cell-coordinate encodings (X10/SGR/urxvt/utf8) ignore it.
        /// Human: the real pointer offset. Controller: `(0, 0)` (cell origin).
        px_off: PixelOffset,
    },
    /// Pointer motion. `buttons == 3` is a no-button hover (motion report code 3);
    /// `buttons != 3` is a held-button drag (kills c). `side` is the cell-half.
    MouseMove {
        buttons: u8,
        row: u16,
        col: u16,
        mods: u8,
        side: SelectionSide,
        /// Sub-cell pixel offset inside the (`row`,`col`) cell for DEC 1016 (see
        /// [`InputEvent::MouseButton::px_off`]).
        px_off: PixelOffset,
    },
    /// A wheel notch / trackpad flick of `lines` lines (kills e: one report per
    /// line when tracking is on, else the viewport scrolls `lines`). `lines` is
    /// clamped to `>= 1` in the seam so a non-positive count can never produce a
    /// silent human/controller asymmetry.
    ///
    /// `dir` is the FOUR-way axis (audit I7), not the old `dir_up: bool`. The
    /// bool could not express a tilt wheel or a horizontal trackpad swipe at
    /// all, so `on_mouse_wheel` dropped those gestures at the door and no app
    /// ever saw xterm's buttons 66/67 — on Windows OR macOS. The horizontal
    /// half is REPORT-ONLY: aterm's own viewport has no horizontal axis, so the
    /// seam turns a horizontal wheel into zero local motion (see the `Wheel`
    /// arm of `seam_egress`), which keeps the tracking-OFF behaviour exactly
    /// what the old early-return produced.
    Wheel {
        dir: aterm_types::mouse::WheelDir,
        lines: i32,
        row: u16,
        col: u16,
        mods: u8,
        /// Sub-cell pixel offset inside the (`row`,`col`) cell for DEC 1016 (see
        /// [`InputEvent::MouseButton::px_off`]).
        px_off: PixelOffset,
    },
    /// Explicit, tracking-agnostic scrollback navigation (the `scroll` verb).
    /// Never emits wheel reports; it only moves the local viewport. A controller
    /// that wants to drive a tracking app's wheel uses `Wheel`/`mouse` instead.
    ScrollView(ScrollIntent),
    /// Paste text as if typed (bracketed when the app enabled DECSET 2004).
    Paste(String),
    /// A geometry change. Re-clamped against `MAX_GRID_*` in the seam.
    ///
    /// `echo_to_window` is a TRANSPORT flag (NOT a `Source` branch): the control
    /// `resize` verb sets it `true` so the seam also asks the window to match the
    /// new grid pixel size (RES-1 — the verb has no window event of its own); the
    /// winit `Resized` handler sets it `false` because the window ALREADY has the
    /// new size and re-`request_inner_size`-ing it would fight an interactive
    /// edge-drag (the RES-1 regression). It is keyed on WHERE the geometry came
    /// from, identical for a human-issued vs controller-issued `resize` verb.
    Resize {
        rows: u16,
        cols: u16,
        echo_to_window: bool,
    },
    /// A WINDOW-PIXEL geometry change: ask the OS window for this inner size and
    /// let the grid follow from the `Resized` event the platform delivers, exactly
    /// as an edge drag does.
    ///
    /// THIS IS NOT `Resize` WITH DIFFERENT UNITS, and the difference is the whole
    /// point. `Resize` applies the grid FIRST and then echoes the matching pixel
    /// size to the window, so by the time the window event arrives the columns
    /// already agree — which means it can never traverse the live-drag path:
    /// `on_resize_throttled` sees no column change, takes the row-only branch, and
    /// the width throttle, its coalescing, and its trailing settle are all
    /// unreachable from the control surface. Every socket-driven resize was
    /// therefore testing the one arm a drag does not use, and the drag arms could
    /// only be exercised by a hand on the window edge.
    ///
    /// This variant inverts that order: nothing is pre-applied, so the grid is
    /// derived from the window event by the same code a human's drag runs. Fire a
    /// few of these in a row and the event loop sees the back-to-back bounds
    /// changes that make a drag a drag. The residual difference from a hand drag is
    /// that AppKit is not in its event-tracking run-loop mode; everything inside
    /// aterm is the same path.
    ResizeWindowPx { width: u32, height: u32 },
    /// Focus gained/lost — DEC 1004 focus reporting (kills j). `true` = focus-in.
    Focus(bool),
}

/// Sub-cell pixel offset of the pointer INSIDE its grid cell, carried on every
/// mouse event so the seam can produce a genuine PIXEL coordinate for DEC 1016
/// (SGR-pixel) mouse mode without re-reading any winit/GUI state.
///
/// `x`/`y` are the device-pixel distance from the cell's top-left corner; combined
/// with the cell origin (`col * cell_w`, `row * cell_h`) and the engine's reported
/// cell pixel size, the seam reconstructs the exact winit cursor pixel. They are
/// IGNORED by every cell-coordinate encoding (X10/SGR/urxvt/utf8) — only the 1016
/// encoder consults them — so a non-pixel session's bytes are unaffected. The Human
/// path fills the real `(x - pad) % cell_w` / per-cell `y`; a Controller (which has
/// no real pointer) sends [`PixelOffset::CELL_ORIGIN`] (`0, 0`), i.e. the cell's
/// top-left, so a controller-driven 1016 press still lands on the right cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PixelOffset {
    /// Horizontal device pixels from the cell's left edge (`0..cell_w`).
    pub x: u16,
    /// Vertical device pixels from the cell's top edge (`0..cell_h`).
    pub y: u16,
}

impl PixelOffset {
    /// The cell's top-left corner — the offset a Controller (no real pointer)
    /// uses, so a 1016 report it drives is exactly the cell origin in pixels.
    pub const CELL_ORIGIN: Self = Self { x: 0, y: 0 };
}

/// Tracking-agnostic scrollback navigation for [`InputEvent::ScrollView`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollIntent {
    /// One screen toward older content.
    Up,
    /// One screen toward the live bottom.
    Down,
    /// `N` lines into history (negative = toward the live bottom).
    By(i32),
    /// Jump to the oldest scrollback.
    Top,
    /// Jump to the live bottom.
    Bottom,
    /// Jump the viewport to the previous (older) shell PROMPT — the nearest
    /// recorded OSC-133 prompt row above the current top visible row (see
    /// [`jump_prompt_target`] for which rows count as prompts). A no-op when
    /// shell integration recorded no older prompt.
    PrevPrompt,
    /// Jump the viewport to the next (newer) shell prompt — the nearest
    /// recorded prompt row below the current top visible row. A no-op when
    /// there is none.
    NextPrompt,
}

/// Resolve the target absolute row for a prompt-to-prompt jump: the nearest
/// recorded PROMPT row strictly ABOVE (`prev = true`) or BELOW (`prev = false`)
/// the current top visible row. `None` when no prompt lies in that direction
/// (an edge, or a bare shell with no integration marks). The single source of
/// truth shared by the seam's `ScrollView` handler and the control-socket
/// `scroll` verb so their prompt-jump semantics can't drift.
///
/// Prompt rows come from BOTH indexes, deliberately. `command_marks()` is
/// populated only by `shell_command_finished` — i.e. only by OSC 133;**D** — so
/// on its own it indexes COMPLETED COMMANDS, not prompts. That is the wrong
/// index for a feature whose name and behaviour are "jump to the prompt": a
/// shell that can mark where its prompt is but cannot mark when a command
/// starts EXECUTING (cmd.exe has no hook between Enter and the command running,
/// so it emits A/B and no C/D) records prompts the engine can see and this
/// reducer could not. `all_blocks()` carries `prompt_start_row` from the instant
/// A arrives, including the in-progress block. For a fully-marked shell the two
/// sets agree row-for-row and the union changes nothing — verified by the
/// bash/zsh/fish/pwsh/wsl path landing on identical targets.
pub(crate) fn jump_prompt_target(t: &Terminal, prev: bool) -> Option<u64> {
    let top = t.grid().top_visible_absolute_row();
    let rows = t
        .command_marks()
        .iter()
        .map(|m| m.prompt_start_row)
        .chain(t.all_blocks().map(|b| b.prompt_start_row));
    if prev {
        rows.filter(|&r| r < top).max()
    } else {
        rows.filter(|&r| r > top).min()
    }
}

/// WHO produced an [`InputEvent`]. AUDIT-ONLY — the seam MUST NOT branch on this
/// (the Tier-1 indistinguishability invariant). `Op` is carried for the §7.5
/// audit log only; it is `Copy`, so `Source` stays `Copy` and the `Wake::Input`
/// drain loop can pass it by value into every event.
///
/// NOTE: design A.2 wrote `Controller { edge: EdgeId }`, but there is NO `EdgeId`
/// type in `aterm-session` (only `SessionId`, `EdgeToken`, `Op`). We carry the
/// `Op` of the OPERATION being performed (the verb's audit class — `ReadScreen` for
/// view control like `scroll`, `WriteInput` for the input verbs), captured at the
/// verb in `control.rs` (`post_input`/`post_input_reply`). It is deliberately NOT
/// read off the connection's `Scope`: the cached connect-time op there can drift from
/// what the verb actually does once the active session swings, which would corrupt the
/// audit trail. The session-owner connection maps to `Controller` too (an owner is
/// still a controller, never `Human`): `Human` is built ONLY by the in-thread winit
/// handlers.
#[derive(Clone, Copy, Debug)]
pub enum Source {
    /// An in-thread winit handler (real keyboard/mouse/focus on this window).
    Human,
    /// A control-socket verb. `op` is the audit class of the OPERATION (the verb's
    /// own op, not the connection's scope). AUDIT-ONLY: captured for a future §7.5
    /// audit log; the seam binds `src` to `_audit` and NEVER reads it for a
    /// behavioural decision (the indistinguishability invariant), so it has no reader.
    Controller {
        #[allow(dead_code)]
        op: Op,
    },
}

/// The reply a reply-bearing verb gets back from the seam. Fire-and-forget
/// callers ignore it. `Copy` so the drain loop can keep the last outcome cheaply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputOutcome {
    /// Applied (bytes written and/or viewport moved).
    Ok,
    /// A `Resize` fell outside `1..=MAX_GRID_*` and was not applied.
    RangeRejected,
    /// The encoded bytes were NOT (fully) written to the PTY — a short write (peer
    /// closed mid-frame) or a hard error (audit finding: the input seam must not
    /// report OK for bytes that did not land; it is the reply-fidelity contract that
    /// `OK` means delivered).
    WriteFailed,
}

/// Whether [`seam_egress`] actually delivered the event's encoded bytes to the PTY.
/// An event that legitimately encodes to NO bytes (a legacy-mode key release, an
/// un-encodable modifier — faithful to what a real terminal does) is [`Full`]: there
/// was nothing to deliver and nothing was lost. Only a short/failed write is
/// [`Failed`].
///
/// [`Full`]: Delivery::Full
/// [`Failed`]: Delivery::Failed
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// Every intended byte reached the PTY (or there were none to write).
    Full,
    /// Every intended byte reached the PTY through an epoch-conditional write.
    /// Carry this token into the next guarded step; reading the epoch afterward
    /// would be racy because it could accidentally adopt a foreign attempt.
    FullAt { epoch: InputEpoch },
    /// A short or failed PTY write — the bytes did not (fully) land.
    Failed,
    /// The immediate actuator path accepted zero bytes and queued nothing.
    BusyZero,
    /// A foreign input attempt invalidated the guarded epoch; zero bytes from
    /// this event were accepted and nothing was queued.
    ConflictZero,
    /// The immediate actuator path handed this prefix to the kernel and queued
    /// no tail.  The mutation is in-doubt and must not be retried.
    PartialInDoubt { accepted: usize },
}

impl Delivery {
    #[must_use]
    pub const fn is_full(self) -> bool {
        matches!(self, Self::Full | Self::FullAt { .. })
    }
}

/// Classify a sink write result against the intended frame length. A partial
/// write (`Ok(n)` with `n < intended`, i.e. the peer closed mid-frame) is a FAILURE
/// just like a hard error — the frame did not land in full. NOTE the wedged-tty
/// nuance: a frame the sink SPILLED (tty input buffer full) reports `Ok(len)` —
/// accepted-for-ordered-delivery — and counts as [`Delivery::Full`] even though
/// the kernel has not consumed it yet; if the session dies before the spill
/// drains, those bytes are dropped with it (exactly like bytes a dead program
/// never read from its tty).
fn delivered(res: std::io::Result<usize>, intended: usize) -> Delivery {
    match res {
        Ok(n) if n == intended => Delivery::Full,
        _ => Delivery::Failed,
    }
}

/// What [`seam_egress`] did with a mouse/wheel event, so `App::input` knows
/// whether the tracking-OFF local fallback (selection gesture / viewport scroll)
/// must still run. The byte-producing decision lives ENTIRELY in `seam_egress`;
/// the viewport/gesture/window side-effects stay in `App::input` (they need the
/// renderer/window the headless byte test does not have).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Egress {
    /// The event produced a tracking report (or had no local fallback): the seam
    /// is done with it. Carries whether the encoded bytes actually reached the PTY
    /// so the reply-bearing caller is told the truth (audit: no false OK).
    Reported(Delivery),
    /// Mouse tracking is OFF: `App::input` must run the local fallback (selection
    /// gesture for a button/move, viewport scroll of `wheel_lines` for a wheel).
    TrackingOff { wheel_lines: i32, wheel_up: bool },
}

/// Resolve the `(x, y)` numbers a mouse report should carry for `term`'s CURRENT
/// encoding: a genuine device-PIXEL coordinate for DEC 1016 (SGR-pixel), else the
/// 0-based grid CELL (`col`, `row`) unchanged.
///
/// For 1016 the pixel coordinate is reconstructed from the cell origin
/// (`col * cell_w`, `row * cell_h`) plus the sub-cell `px_off` the frontend
/// measured from the real winit cursor — the engine's `cell_pixel_size()` (the
/// frontend's reported font metrics) supplies the cell size. The result is 0-based;
/// `encode_mouse` (`encode_sgr`) adds the spec's `+1`, so a pointer at the very
/// top-left pixel of cell (0,0) reports `1;1`. Saturating arithmetic keeps a huge
/// grid from wrapping (the SGR field is `u16`-wide as the rest of the pipeline).
///
/// This is the SOLE reader of `mouse_encoding()` for coordinate selection; it runs
/// under the caller's existing `term_lock`, so it adds no extra mode-read window.
pub(crate) fn report_coords(t: &Terminal, col: u16, row: u16, px_off: PixelOffset) -> (u16, u16) {
    use aterm_types::mouse::MouseEncoding;
    if t.mouse_encoding() == MouseEncoding::SgrPixel {
        let (cw, ch) = t.cell_pixel_size();
        let px = col
            .saturating_mul(cw)
            .saturating_add(px_off.x.min(cw.saturating_sub(1)));
        let py = row
            .saturating_mul(ch)
            .saturating_add(px_off.y.min(ch.saturating_sub(1)));
        (px, py)
    } else {
        (col, row)
    }
}

/// Ceiling on the number of times ONE wheel event may write its encoded bytes.
/// Both bursts the seam can produce are bounded by it: the per-line mouse reports
/// under a tracking app, and the DEC-1007 alt-scroll arrow presses, whose count is
/// `lines` times the platform's lines-per-detent and so can exceed `lines` on its
/// own (Windows' "One screen at a time" multiplies by the viewport height). 512
/// covers a large flick of many screens; past that a single event is flooding the
/// PTY, not scrolling.
///
/// SOURCE-BLINDNESS. The report burst is clamped at the SEAM's `lines`
/// normalization, not (only) at the `mouse` verb's `lines=N` parse — the verb's
/// clamp is `control_input::MAX_WHEEL_LINES`, which IS this constant, so a
/// controller was already bounded while a HUMAN gesture was not. That gap became
/// live exposure with the horizontal axis (audit I7): a trackpad's pixel deltas
/// are divided by the cell WIDTH, so one momentum event can bank far more notches
/// sideways than the vertical twin ever did. Clamping where both sources converge
/// makes the ceiling structural instead of a property of one caller.
pub(crate) const MAX_WHEEL_BURST: i32 = 512;

/// How far a wheel gesture of `notch_lines` DETENTS travels, in lines, once the
/// platform's lines-per-detent is applied. `page_rows` is the viewport height, used
/// only for the "One screen at a time" setting.
///
/// WINDOWS ONLY (identity elsewhere). winit's Win32 backend reports exactly ±1.0
/// `LineDelta` per detent, but on Windows a detent is not one line: it is
/// `SPI_GETWHEELSCROLLLINES` lines — default **3**, the Mouse-settings slider, the
/// distance every other window on the desktop travels. macOS and X11 need no such
/// term: AppKit folds the user's scroll speed into `scrollingDeltaY` before winit
/// sees it, and X11's button-4/5 ARE one line by convention.
///
/// THE TWO CALLERS ARE THE TWO SURFACES THE USER SCROLLS WITHOUT AN APP'S HELP:
/// the local scrollback viewport (`App::input_wheel`), and the alternate-scroll
/// (DEC 1007) arrow synthesis in `seam_egress`, which is how `less`, `man` and
/// `git log` scroll. The mouse-TRACKING path deliberately does NOT call this: there
/// the app receives real wheel reports and applies its own notch->lines conversion
/// (vim's `mousescroll`, default 3), so scaling would compound to nine lines a
/// notch.
#[cfg(windows)]
pub(crate) fn wheel_platform_lines(notch_lines: i32, page_rows: u16) -> i32 {
    wheel_scaled_lines(
        notch_lines,
        crate::platform_win::wheel_notch_scroll(),
        page_rows,
    )
}

/// The non-Windows twin of [`wheel_platform_lines`]: the platform already delivered
/// the user's scroll speed in the delta, so the gesture's line count is final.
#[cfg(not(windows))]
pub(crate) fn wheel_platform_lines(notch_lines: i32, _page_rows: u16) -> i32 {
    notch_lines
}

/// Where ONE wheel event goes — the seam's routing decision, factored out of
/// `seam_egress` so the precedence is a pure, cross-platform-testable truth
/// table (the byte production stays in the seam, under its single term-lock).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WheelRoute {
    /// An app is tracking the mouse: encode wheel reports for it.
    Report,
    /// Alt screen + DEC 1007, tracking off: synthesize arrow-key presses.
    AltScroll,
    /// aterm's own scrollback viewport (`Egress::TrackingOff`).
    Viewport,
}

/// Resolve a wheel's [`WheelRoute`] from the three facts the seam reads under
/// its term lock. THE ORDER IS THE CONTRACT:
///
/// 1. `shift_held` wins over EVERYTHING (audit I12). Shift+wheel is the user's
///    explicit "scroll ATERM, not the app" gesture — the xterm convention whose
///    KEYBOARD half aterm already ships (`scrollback_chord`, Shift+PgUp/PgDn,
///    citing "the xterm / Terminal.app convention"). Without it there is NO
///    wheel gesture that moves the viewport over fzf / a mouse-enabled REPL.
///    Checked BEFORE the tracking test so the shifted event can never reach
///    `encode_mouse_wheel` (which would fold SHIFT_MASK into the report byte —
///    the bypass must not leak a shifted report to the app), and before the
///    DEC-1007 arm for the same reason xterm's is: Shift reserves the wheel for
///    the terminal, full stop. The fallback path never reads the modifier, so
///    nothing downstream needs to "strip" Shift, and the platform
///    lines-per-detent multiplier still applies (`wheel_viewport_lines` runs on
///    every `TrackingOff` wheel).
/// 2. `alt_local` — Option/Alt held on the MAIN screen (SELECTION CUSTODY
///    Phase 2): the wheel scrolls THIS terminal's scrollback even while an app
///    tracks the mouse. Option is aterm's OWN established override modifier
///    under tracking, not an import: `press_starts_selection` (`app_mouse.rs`,
///    audit M6) already lets Option-click start a local selection "so a PID can
///    be copied out of htop" — one modifier, one meaning; this is the wheel
///    half of the same gesture. The CALLER computes the main-screen scoping
///    (`ALT_MASK` held AND not alt screen): the alt screen is built with zero
///    scrollback, so a bypass there buys nothing while costing the DEC-1007
///    arrows that less/man/git-log scroll by — with Alt on the alt screen the
///    wheel behaves exactly as it always has.
/// 3. Mouse tracking: the app grabbed the mouse, it gets real reports.
/// 4. Alternate scroll (DEC 1007, alt screen): arrows for less/man/git-log.
/// 5. Otherwise: the local scrollback viewport.
///
/// REJECTED — stripping Shift in `on_mouse_wheel` (the GUI handler) instead:
/// that would bypass for the Human path only, silently diverging the controller
/// `mouse` verb (the seam is deliberately source-blind; a policy split keyed on
/// the builder is exactly the divergence the A.7 invariant forbids). Also
/// REJECTED — a platform-conditional bypass modifier (the `link_modifier_held`
/// shape): the convention is xterm's own and holds on every platform aterm
/// ships; a per-platform split would buy nothing but a third behaviour matrix.
/// (macOS trackpads convert a Shift+swipe to a HORIZONTAL delta before winit
/// sees it, so this arm is naturally rare there — but a Shift+notch on a real
/// mouse wheel behaves identically everywhere.)
pub(crate) fn wheel_route(
    shift_held: bool,
    alt_local: bool,
    tracking: bool,
    alt_scroll: bool,
) -> WheelRoute {
    if shift_held {
        return WheelRoute::Viewport;
    }
    if alt_local {
        return WheelRoute::Viewport;
    }
    if tracking {
        return WheelRoute::Report;
    }
    if alt_scroll {
        return WheelRoute::AltScroll;
    }
    WheelRoute::Viewport
}

/// The four FACTS [`wheel_route`] decides on, read from one engine under the
/// caller's lock. Only reads — the policy is still entirely in `wheel_route`.
///
/// Split out of [`seam_egress`] so the derivation is reachable from a test on
/// EVERY platform. The seam's own wheel tests used to need a POSIX pipe fd for
/// `SinkWriter` and were therefore `#[cfg(unix)]`, which left the SELECTION
/// CUSTODY Phase-2 Option override — the item the design calls load-bearing,
/// "without this, Phase 4 is unreachable under any mouse-owning TUI" — unpinned
/// on Windows, the platform where Alt is most likely to collide with something
/// else. (The byte capture is cross-platform now, so the seam's own wheel tests
/// run on Windows too; this pure derivation stays because a route test that needs
/// no PTY at all is still the cheaper and sharper fence.) `alt_local`'s
/// main-screen scoping lives here because it is a FACT about the engine (the alt
/// screen carries no scrollback), not a policy choice.
pub(crate) fn wheel_route_for(t: &Terminal, mods: u8) -> WheelRoute {
    wheel_route(
        mods & aterm_types::mouse::SHIFT_MASK != 0,
        mods & aterm_types::mouse::ALT_MASK != 0 && !t.is_alternate_screen(),
        t.mouse_tracking_enabled(),
        t.is_alternate_screen() && t.modes().alternate_scroll,
    )
}

/// The pure arithmetic of [`wheel_platform_lines`], split out so the multiply can be
/// tested without a live `SystemParametersInfoW` (whose answer belongs to whoever's
/// machine runs the suite, and so can never be asserted).
///
/// [`WheelNotch::Page`] is the slider's "One screen at a time" position: a detent is
/// then a PAGE, sized as the viewport so a wheel and PgUp/PgDn cannot disagree about
/// what a page is. `Lines(0)` — "wheel scrolling off" in Mouse settings — is
/// honoured as zero rather than clamped up to one: a user who turned the wheel off
/// means it.
#[cfg(windows)]
fn wheel_scaled_lines(
    notch_lines: i32,
    notch: crate::platform_win::WheelNotch,
    page_rows: u16,
) -> i32 {
    let per_notch = match notch {
        crate::platform_win::WheelNotch::Lines(n) => i32::try_from(n).unwrap_or(i32::MAX),
        crate::platform_win::WheelNotch::Page => i32::from(page_rows).max(1),
    };
    notch_lines.saturating_mul(per_notch)
}

/// How [`seam_egress`] hands the encoded bytes to the [`SinkWriter`] — a TRANSPORT
/// knob keyed on the CALLING THREAD, never on who produced the event (cf.
/// `echo_to_window` and the `resize` arm, which key on WHERE the event came from,
/// not on [`Source`]). It NEVER changes WHICH bytes are produced, so the Tier-1
/// `bytes_human_eq_controller` byte-equality invariant holds for either variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressMode {
    /// The UI event-loop thread: use the non-parking egress so a wedged foreground
    /// can NEVER park the loop that serves rendering + input for every window/tab.
    Interactive,
    /// An expendable egress thread (the per-session egress-order writer thread, the
    /// detached paste writer, the cross-session control thread): use the blocking,
    /// `SPILL_CAP`-enforcing egress, so a machine-rate producer into a wedged
    /// foreground feels backpressure on THIS thread instead of growing the spill
    /// without bound. MUST NOT be used on the UI thread.
    Backpressured,
    /// A guarded actuator call: either write one bounded frame to the kernel
    /// immediately or fail without spilling it for later delivery.  This mode is
    /// non-parking and preserves the generation checked immediately before the
    /// action; [`Delivery::Failed`] means the caller must stop and treat the
    /// action as refused/in-doubt, never retry it blindly.
    #[cfg(test)]
    TryImmediate,
}

/// Deliver `bytes` to `sink` per `mode`. Transport only: the bytes are identical
/// either way (see [`EgressMode`]); only the parking discipline differs.  The
/// immediate mode retains the zero-vs-partial distinction needed by the durable
/// actuator; ordinary modes preserve their existing `Full`/`Failed` contract.
fn emit(sink: &SinkWriter, mode: EgressMode, bytes: &[u8]) -> Delivery {
    match mode {
        EgressMode::Interactive => delivered(sink.write_frame_nonparking(bytes), bytes.len()),
        EgressMode::Backpressured => delivered(sink.write_frame(bytes), bytes.len()),
        #[cfg(test)]
        EgressMode::TryImmediate => match sink.try_write_frame_immediate(bytes) {
            ImmediateWrite::Full => Delivery::Full,
            ImmediateWrite::BusyZero => Delivery::BusyZero,
            ImmediateWrite::ConflictZero => Delivery::ConflictZero,
            ImmediateWrite::PartialInDoubt { accepted } => Delivery::PartialInDoubt { accepted },
        },
    }
}

/// THE source-blind byte-producing core of the seam (design A.2 / A.7). It is the
/// SOLE reader of `keyboard_mode()`/`mouse_tracking_enabled()` and the SOLE caller
/// of `encode_key_with_layout` / the `encode_mouse_*` family / `encode_committed_
/// text` / `format_paste` / the focus-report egress, reading the relevant mode
/// ONCE per event under a single `term_lock`, ending at the `mode`-selected
/// [`emit`] (`Interactive` = non-parking on the UI thread; `Backpressured` =
/// blocking + `SPILL_CAP` on an expendable thread; `TryImmediate` = guarded,
/// non-spilling actuator egress). Sole BYTE-PRODUCING reader:
/// the predictive-echo gate reads the narrow
/// `Terminal::kitty_suppresses_predictive_echo()` projection too, but only to
/// decide whether a local guess may PAINT — a display-only read that can never
/// diverge the wire bytes.
///
/// CRITICAL (the indistinguishability invariant): this function takes NO
/// [`Source`] — it is STRUCTURALLY impossible for it to branch on who produced
/// the event, so a Human and a Controller feeding the SAME `InputEvent` get
/// byte-identical output. The Tier-1 `bytes_human_eq_controller` test proves it,
/// and the `Buggy` mutant (which DOES take a source flag) proves the test has
/// teeth. [`EgressMode`] is a TRANSPORT knob (keyed on the calling thread, not the
/// source) and never touches the produced bytes, so the invariant is preserved.
///
/// Only the byte-producing arms are handled here; the viewport/gesture/clipboard/
/// blink/snap/resize side-effects (which need the renderer + window + gesture
/// state) stay in `App::input`, which calls this and then runs those.
pub fn seam_egress(
    term: &Mutex<Terminal>,
    sink: &SinkWriter,
    ev: &InputEvent,
    mode: EgressMode,
) -> Egress {
    match ev {
        InputEvent::Key {
            key,
            mods,
            base_layout,
            event_type,
        } => {
            let bytes = {
                let t = term_lock(term);
                let mode = t.keyboard_mode();
                aterm_types::keyboard::encode_key_with_layout(
                    key,
                    *mods,
                    mode,
                    *event_type,
                    *base_layout,
                )
            };
            let d = if bytes.is_empty() {
                Delivery::Full // faithful no-op (e.g. legacy release): nothing to deliver
            } else {
                emit(sink, mode, &bytes)
            };
            Egress::Reported(d)
        }
        InputEvent::Text(text) => {
            let mut d = Delivery::Full;
            if !text.is_empty() {
                let out = {
                    let mode = term_lock(term).keyboard_mode();
                    crate::keymap::encode_committed_text(text, mode)
                };
                if !out.is_empty() {
                    d = emit(sink, mode, &out);
                }
            }
            Egress::Reported(d)
        }
        InputEvent::KeySequence(bytes) => {
            // aterm INPUT POLICY: raw user-chosen bytes go to the PTY VERBATIM — no
            // keyboard-mode read, no encoder. This is the whole point: the chord
            // sends exactly what the user mapped, overriding the default encoding.
            let d = if bytes.is_empty() {
                Delivery::Full
            } else {
                emit(sink, mode, bytes)
            };
            Egress::Reported(d)
        }
        InputEvent::MouseButton {
            button,
            pressed,
            row,
            col,
            mods,
            px_off,
            ..
        } => {
            let report = {
                let t = term_lock(term);
                if t.mouse_tracking_enabled() {
                    // DEC 1016 (SGR-pixel) reports a genuine PIXEL coordinate; every
                    // other encoding reports the cell. `report_coords` resolves which
                    // under the SAME lock that reads the mode (no extra read window).
                    let (rx, ry) = report_coords(&t, *col, *row, *px_off);
                    if *pressed {
                        Some(t.encode_mouse_press(button.code(), rx, ry, *mods))
                    } else {
                        Some(t.encode_mouse_release(button.code(), rx, ry, *mods))
                    }
                } else {
                    None
                }
            };
            match report {
                Some(bytes) => {
                    let d = match bytes {
                        Some(b) => emit(sink, mode, &b),
                        None => Delivery::Full,
                    };
                    Egress::Reported(d)
                }
                None => Egress::TrackingOff {
                    wheel_lines: 0,
                    wheel_up: false,
                },
            }
        }
        InputEvent::MouseMove {
            buttons,
            row,
            col,
            mods,
            px_off,
            ..
        } => {
            let report = {
                let t = term_lock(term);
                if t.mouse_tracking_enabled() {
                    let (rx, ry) = report_coords(&t, *col, *row, *px_off);
                    Some(t.encode_mouse_motion(*buttons, rx, ry, *mods))
                } else {
                    None
                }
            };
            match report {
                Some(bytes) => {
                    let d = match bytes {
                        Some(b) => emit(sink, mode, &b),
                        None => Delivery::Full,
                    };
                    Egress::Reported(d)
                }
                None => Egress::TrackingOff {
                    wheel_lines: 0,
                    wheel_up: false,
                },
            }
        }
        InputEvent::Wheel {
            dir,
            lines,
            row,
            col,
            mods,
            px_off,
        } => {
            // The invariant lives HERE: clamp `lines` to `1..=MAX_WHEEL_BURST` so
            // (low) a non-positive count (a future verb/grammar bug) cannot
            // silently emit zero reports for one source and N for another, and
            // (high) one event cannot flood the PTY with reports. `on_mouse_wheel`
            // already guarantees >= 1 and the `mouse` verb already clamps its
            // `lines=N` to the same ceiling; doing BOTH here makes the bound
            // structural for every caller instead of a property of one of them.
            let lines = (*lines).clamp(1, MAX_WHEEL_BURST);
            // Decide the wheel's egress under the SINGLE term_lock window. Three
            // source-blind outcomes (Human and Controller converge):
            //   Write     — emit the bytes `repeat` times: a mouse-wheel report ONCE
            //               PER LINE when an app is tracking, OR (alt screen + DEC
            //               1007, audit M5) a synthesized arrow key so a wheel scrolls
            //               less/man/vim — that one scaled by the platform's
            //               lines-per-detent, see the arm below;
            //   Swallow   — Reported with nothing to write (X10 mode-9 press-only, or
            //               an empty key encoding): the wheel is CONSUMED, the local
            //               viewport must NOT move;
            //   Fallback  — tracking off and not alt-scroll, OR Shift held (the
            //               I12 bypass — see `wheel_route`): `App::input` scrolls
            //               the local scrollback viewport (`Egress::TrackingOff`)
            //               — by ZERO lines on the horizontal axis, which has no
            //               viewport to move (audit I7, see the Fallback arm).
            enum WheelPlan {
                Write { bytes: Vec<u8>, repeat: i32 },
                Swallow,
                Fallback,
            }
            let plan = {
                let t = term_lock(term);
                // The precedence (Shift bypass > Alt local-override > tracking >
                // alt-scroll > viewport) lives in `wheel_route` — pure, so the
                // table is pinned by tests on every platform. The facts are read
                // HERE, under this lock; the Alt override's main-screen scoping
                // (no scrollback on the alt screen — see the contract) is one of
                // those facts, not policy.
                let route = wheel_route_for(&t, *mods);
                if route == WheelRoute::Report {
                    let (rx, ry) = report_coords(&t, *col, *row, *px_off);
                    match t.encode_mouse_wheel(*dir, rx, ry, *mods) {
                        // EXACTLY one report per line, NEVER scaled by the platform's
                        // lines-per-detent: the app grabbed the mouse and does its own
                        // notch->lines conversion (vim's `mousescroll`, default 3).
                        // Multiplying here would hand vim three notches to multiply,
                        // i.e. nine lines a detent on Windows.
                        Some(b) => WheelPlan::Write {
                            bytes: b,
                            repeat: lines,
                        },
                        None => WheelPlan::Swallow, // X10 (mode 9) is press-only
                    }
                } else if route == WheelRoute::AltScroll
                    && let Some(up) = dir.vertical_up()
                {
                    // Alternate scroll (DEC mode 1007, audit M5): the alt screen has no
                    // scrollback, so when the app did NOT grab the mouse the wheel
                    // becomes arrow-key PRESSES — how less/man/git-log scroll under a
                    // wheel. Encoded through the engine's LIVE keyboard mode so DECCKM
                    // (SS3 arrows) and the kitty forms stay exact (never a hardcoded
                    // ESC[A).
                    //
                    // The COUNT is the platform's lines-per-detent, not one: this is a
                    // pager with no wheel of its own, so aterm owes it the same
                    // distance the local viewport gets (`SPI_GETWHEELSCROLLLINES`,
                    // default 3, on Windows — identity elsewhere). xterm and Windows
                    // Terminal both send N arrows here. Unlike the tracking arm above
                    // there is nothing downstream to multiply again: `less` moves one
                    // line per ArrowDown, full stop. Bounded by [`MAX_WHEEL_BURST`] for
                    // the reason the `mouse` verb is: "One screen at a time"
                    // (WHEEL_PAGESCROLL) times a large flick would otherwise let one
                    // event flood the PTY.
                    //
                    // VERTICAL ONLY — the `vertical_up()` guard on this arm. REJECTED:
                    // synthesizing Left/Right arrows for a horizontal flick. DEC 1007's
                    // whole premise is that the alt screen has no scrollback, so the
                    // wheel owes the PAGER the motion it would have made; a pager's
                    // left/right arrows are not a horizontal scroll — in `less` they are
                    // (by default) unbound-ish/positional, and in a shell's alt-screen
                    // TUI an ArrowRight is a CURSOR MOVE or a menu entry. xterm does not
                    // do it either: its alternateScroll only maps the vertical pair. A
                    // horizontal flick over a non-tracking alt-screen app is therefore
                    // nothing, which is exactly what it was before this widening.
                    let arrow = if up {
                        NamedKey::ArrowUp
                    } else {
                        NamedKey::ArrowDown
                    };
                    let bytes = encode_key_with_event(
                        &Key::Named(arrow),
                        Modifiers::empty(),
                        t.keyboard_mode(),
                        KeyEventType::Press,
                    );
                    if bytes.is_empty() {
                        WheelPlan::Swallow
                    } else {
                        WheelPlan::Write {
                            bytes,
                            repeat: wheel_platform_lines(lines, t.rows()).clamp(0, MAX_WHEEL_BURST),
                        }
                    }
                } else {
                    WheelPlan::Fallback
                }
            };
            match plan {
                WheelPlan::Write { bytes: b, repeat } => {
                    // One report / keypress PER unit of `repeat` (kills divergence e —
                    // `repeat` is computed source-blind, from the terminal's modes and
                    // the platform's setting, never from `_audit`).
                    let mut d = Delivery::Full;
                    for _ in 0..repeat {
                        let attempt = emit(sink, mode, &b);
                        if !attempt.is_full() {
                            d = attempt; // any short/failed write fails the lot
                        }
                    }
                    Egress::Reported(d)
                }
                WheelPlan::Swallow => Egress::Reported(Delivery::Full),
                // The local-viewport fallback is VERTICAL-ONLY (audit I7). A grid
                // has one axis: `display_offset` moves through scrollback and
                // there is no horizontal viewport to pan, so a tilt notch or a
                // horizontal trackpad swipe reports ZERO lines and `input_wheel`
                // does nothing with it — byte-identical AND motion-identical to
                // the old `on_mouse_wheel` early-return, which is the behaviour
                // the audit explicitly ruled CORRECT with tracking off. Feeding
                // `lines` through here instead would resurrect the phantom
                // scroll-DOWN that guard was added to fix.
                WheelPlan::Fallback => Egress::TrackingOff {
                    wheel_lines: if dir.is_horizontal() { 0 } else { lines },
                    wheel_up: dir.vertical_up().unwrap_or(false),
                },
            }
        }
        InputEvent::Paste(text) => {
            let out = term_lock(term).format_paste(text);
            let d = if out.is_empty() {
                Delivery::Full
            } else {
                emit(sink, mode, &out)
            };
            Egress::Reported(d)
        }
        InputEvent::Focus(focused) => {
            // SOLE focus-report egress: ESC[I / ESC[O under DEC 1004, byte-identical
            // to the engine's `encode_focus_state`.
            let mut d = Delivery::Full;
            if term_lock(term).focus_reporting_enabled() {
                let seq: &[u8] = if *focused { b"\x1b[I" } else { b"\x1b[O" };
                d = emit(sink, mode, seq);
            }
            Egress::Reported(d)
        }
        // ScrollView / Resize / ResizeWindowPx produce no PTY bytes here;
        // `App::input` handles their (viewport / geometry) side-effects directly.
        // `ResizeWindowPx` in particular writes NOTHING to the engine: it only asks
        // the window for a size, and the PTY learns the new geometry from the
        // platform's `Resized` event like it does for a drag.
        InputEvent::ScrollView(_)
        | InputEvent::Resize { .. }
        | InputEvent::ResizeWindowPx { .. } => Egress::Reported(Delivery::Full),
    }
}

/// Escape a dragged-and-dropped file path so it survives as ONE shell argument
/// when inserted at the prompt. Pure cfg dispatch: POSIX shells escape with
/// backslashes ([`shell_escape_path_posix`]); on Windows the spawned shell is
/// pwsh/powershell/cmd (the `aterm-pty` selection order), where backslash is the
/// PATH SEPARATOR, not an escape — backslash-escaping would split
/// `C:\Program Files\a.txt` at the space AND double every separator, so Windows
/// uses double-quote wrapping instead ([`shell_escape_path_windows`]). The
/// result carries NO trailing space and NO newline; the drop site appends the
/// single separating space (so an N-file drop concatenates to `p1 p2 p3 `) and
/// routes the text through the normal paste seam, which never executes it.
pub(crate) fn shell_escape_path(path: &str) -> String {
    #[cfg(windows)]
    return shell_escape_path_windows(path);
    #[cfg(not(windows))]
    return shell_escape_path_posix(path);
}

/// Windows drop quoting: wrap the path in double quotes — the ONE quoting form
/// PowerShell and cmd share — doubling any embedded `"` (`""` is the escaped
/// quote in both; defensive only, since NTFS forbids `"` and control chars in
/// names). No backslash escaping: `\` is the path separator and passes through
/// verbatim. A path of only inert chars (alphanumerics incl. non-ASCII, `\ / :
/// . - _`) is returned unchanged so tame paths paste clean; anything else —
/// space, `( )` , `,` (a PowerShell array separator!), `& | < > ; @ $ ' ~` … —
/// triggers the quotes. Known residue: cmd expands `%VAR%` and PowerShell
/// expands `$name` even inside double quotes; no single quoting works in both
/// shells, and such names are vanishingly rare in droppable paths. Defined
/// winit/fs-free so it is unit-tested on every target.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn shell_escape_path_windows(path: &str) -> String {
    let inert = |c: char| c.is_alphanumeric() || matches!(c, '\\' | '/' | ':' | '.' | '-' | '_');
    if path.chars().all(inert) {
        return path.to_string();
    }
    let mut out = String::with_capacity(path.len() + 2);
    out.push('"');
    for c in path.chars() {
        if c == '"' {
            out.push('"');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// POSIX drop escaping: backslash-escape the path — byte-for-byte what iTerm2
/// does on a file drop. The set is iTerm2's `+[NSString
/// shellEscapableCharacters]` (`NSStringITerm.m`) plus the CR/LF its
/// dropped-file escaper adds
/// (`-stringWithEscapedShellCharactersIncludingNewlines:YES`): each listed byte
/// is prefixed with a backslash so spaces, quotes, globs, command substitution,
/// history-expansion, redirections, pipes, etc. cannot break the path into
/// multiple words or run as code. Backslash itself is in the set, so a literal
/// `\` becomes `\\`; a single forward pass can't double-escape because it reads
/// only INPUT chars, never the backslashes it just emitted.
///
/// Everything outside the set — `/`, `.`, `-`, `_`, `,`, `%`, `:`, letters,
/// digits, and all multibyte UTF-8 — is shell-inert and passes through verbatim,
/// so a plain path is returned unchanged. Defined winit/fs-free so it is
/// unit-tested on every target.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn shell_escape_path_posix(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 8);
    for c in path.chars() {
        if matches!(
            c,
            '\\' | ' '
                | '('
                | ')'
                | '"'
                | '&'
                | '\''
                | '!'
                | '$'
                | '<'
                | '>'
                | ';'
                | '|'
                | '*'
                | '?'
                | '['
                | ']'
                | '#'
                | '`'
                | '\t'
                | '{'
                | '}'
                | '^'
                | '+'
                | '='
                | '@'
                | '~'
                | '\r'
                | '\n'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Build the terminal INSERTION for a list of file paths — the clipboard
/// CF_HDROP paste (audit S9: Ctrl+C a file in Explorer, Ctrl+V in aterm) —
/// reusing the drag-and-drop contract EXACTLY (`App::drop_file`): each path
/// shell-escaped ([`shell_escape_path`]) and given ONE trailing space, so a
/// multi-file copy reproduces iTerm's space-joined `p1 p2 p3 ` and the
/// insertion ends in a space, NOT a newline — nothing is executed. Empty paths
/// contribute nothing (mirroring `drop_file`'s empty-path early return).
/// Winit/fs-free so the join contract is unit-tested on every target; the
/// escaping itself is pinned by the `shell_escape_path_*` suites above it.
/// Its one production caller is the Windows `CF_HDROP` arm of
/// `App::paste_clipboard_into`, hence the off-Windows dead-code allowance
/// (the `shell_escape_path_posix` shape).
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub(crate) fn paths_paste_insertion(paths: &[String]) -> String {
    let mut out = String::new();
    for p in paths {
        if p.is_empty() {
            continue;
        }
        out.push_str(&shell_escape_path(p));
        out.push(' ');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_core::terminal::Terminal;
    use aterm_types::mouse::WheelDir;
    #[cfg(unix)]
    use std::io::Read;
    #[cfg(unix)]
    use std::os::fd::FromRawFd;
    use std::sync::Arc;

    /// A live PTY-master stand-in whose bytes the test can read back: the harness
    /// every byte-level test in this module drives [`seam_egress`] against.
    ///
    /// WHY THIS TYPE EXISTS. Nearly half of this module's byte-level tests used to
    /// be `#[cfg(unix)]` — including `sgr_pixel_1016_reports_true_pixel_coordinate`,
    /// which pins the exact DEC 1016 contract the CELL-PX-1 defect broke, so that
    /// regression could not have been caught on Windows at all. Nothing about the
    /// SUBSTANCE of those tests is POSIX: `seam_egress` is source-blind, platform-
    /// blind byte production. Only the HARNESS was — it needed `pipe(2)` and a raw
    /// fd. Both platforms can supply a readable master, so both now do, and the
    /// tests run everywhere:
    ///
    /// * POSIX — `pipe(2)`; the `SinkWriter`'s master IS the write-end fd.
    /// * Windows — a `SinkWriter`'s master is not a handle at all but a key into
    ///   aterm-pty's ConPTY session registry, so the equivalent is to REGISTER a
    ///   session whose input handle is a pipe this test holds the read end of.
    ///   [`aterm_pty::adopt_handoff`] is exactly that public constructor (it exists
    ///   for the DefTerm hand-off: "here are the handles, make a session"), so the
    ///   Windows twin drives the SAME production `write_frame` path — a real
    ///   `WriteFile` on a real kernel pipe — never a stub.
    ///
    /// The remaining `#[cfg(unix)]` tests below are the ones whose ASSERTIONS are
    /// POSIX (alternate-scroll ARROW COUNTS, which Windows multiplies by
    /// `SPI_GETWHEELSCROLLLINES`; POSIX backslash path escaping), each marked with
    /// the reason at its own gate.
    #[cfg(unix)]
    struct CaptureSink {
        /// Read end; `-1` once [`Self::drain`] has handed it to a `File`.
        read: i32,
        /// Write end — the `SinkWriter`'s master. `-1` once drained (the drain
        /// closes it so the read can reach a real EOF).
        write: i32,
    }

    #[cfg(unix)]
    impl CaptureSink {
        fn new() -> Self {
            let mut fds = [0i32; 2];
            assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe(2)");
            Self {
                read: fds[0],
                write: fds[1],
            }
        }

        /// The master a `SinkWriter` is built over.
        fn master(&self) -> i32 {
            self.write
        }

        /// Every byte the sink has written. Closes the write end first so the read
        /// runs to a genuine EOF; one drain per capture is all any caller needs.
        fn drain(&mut self) -> Vec<u8> {
            if self.write >= 0 {
                unsafe { libc::close(self.write) };
                self.write = -1;
            }
            let mut buf = Vec::new();
            let mut reader = unsafe { std::fs::File::from_raw_fd(self.read) };
            self.read = -1; // the `File` owns it now and closes it on drop
            reader.read_to_end(&mut buf).expect("read pipe");
            buf
        }
    }

    #[cfg(unix)]
    impl Drop for CaptureSink {
        fn drop(&mut self) {
            for fd in [self.read, self.write] {
                if fd >= 0 {
                    unsafe { libc::close(fd) };
                }
            }
        }
    }

    /// See the POSIX twin's docs. On Windows the capture is an anonymous pipe
    /// registered as a ConPTY session's INPUT handle, so `SinkWriter::write_frame`
    /// reaches it through the ordinary `aterm_pty::write_some` path.
    #[cfg(windows)]
    struct CaptureSink {
        /// The registry key `SinkWriter` writes through.
        master: i32,
        /// Read end of the pipe the session writes into (ours to close).
        read: isize,
        /// Our handle on the event the session holds as its "client process".
        /// Signalling it releases the session's waiter thread immediately instead
        /// of leaving it parked on the close grace.
        stop: isize,
    }

    #[cfg(windows)]
    impl CaptureSink {
        /// Pipe buffer. Deliberately far larger than the system default: the
        /// harness reads only AFTER `seam_egress` returns, and a burst test
        /// (`MAX_WHEEL_BURST` reports) writing past a 4 KiB buffer with no reader
        /// would block the blocking `WriteFile` forever — a hang instead of a
        /// failure, which is a much worse signal. The largest burst the seam can
        /// produce for ONE event is `MAX_WHEEL_BURST` (512) mouse reports of ten
        /// bytes each — about 5 KiB — so 256 KiB is ~50x headroom without asking
        /// the kernel for a megabyte on every capture.
        const PIPE_BYTES: u32 = 256 * 1024;

        fn new() -> Self {
            let (mut read, mut write): (isize, isize) = (0, 0);
            // SAFETY: two out-params, default security attributes, explicit size.
            let ok = unsafe {
                winapi::CreatePipe(&mut read, &mut write, std::ptr::null_mut(), Self::PIPE_BYTES)
            };
            assert_ne!(ok, 0, "CreatePipe for the egress capture");
            // ONE event object under TWO handles, via the name: the session takes
            // one as its `client_process` (the waiter only needs something
            // waitable, and `adopt_handoff` REFUSES a null one), and we keep the
            // other so `drop` can wake the waiter. A per-capture unique name so two
            // concurrent tests can never share — or pre-signal — each other's.
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let nth = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let name: Vec<u16> = format!("aterm-egress-capture-{}-{nth}\0", std::process::id())
                .encode_utf16()
                .collect();
            // SAFETY: NULL attrs; manual-reset, initially unsignaled; NUL-terminated
            // wide name. The second call opens the SAME object (documented
            // `CreateEventW` behaviour for an existing name).
            let ours = unsafe { winapi::CreateEventW(std::ptr::null_mut(), 1, 0, name.as_ptr()) };
            let theirs = unsafe { winapi::CreateEventW(std::ptr::null_mut(), 1, 0, name.as_ptr()) };
            assert!(ours != 0 && theirs != 0, "CreateEventW for the capture");
            let spawned = aterm_pty::adopt_handoff(write, 0, 0, theirs)
                .expect("register the capture session");
            Self {
                master: spawned.master,
                read,
                stop: ours,
            }
        }

        fn master(&self) -> i32 {
            self.master
        }

        /// Every byte the sink has written. `PeekNamedPipe` first: the session is
        /// still live (its write end is open), so a read-to-EOF would block
        /// forever — the byte COUNT already in the pipe is the exact answer,
        /// because `write_frame` on Windows is a synchronous blocking `WriteFile`
        /// that has fully returned before `seam_egress` did.
        fn drain(&mut self) -> Vec<u8> {
            let mut avail: u32 = 0;
            // SAFETY: live read handle; every optional out-param is NULL-allowed.
            let peeked = unsafe {
                winapi::PeekNamedPipe(
                    self.read,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut avail,
                    std::ptr::null_mut(),
                )
            };
            if peeked == 0 || avail == 0 {
                return Vec::new();
            }
            let mut buf = vec![0u8; avail as usize];
            let mut got: u32 = 0;
            // SAFETY: `buf` holds `avail` writable bytes; synchronous read.
            let ok = unsafe {
                winapi::ReadFile(
                    self.read,
                    buf.as_mut_ptr(),
                    avail,
                    &mut got,
                    std::ptr::null_mut(),
                )
            };
            assert_ne!(ok, 0, "ReadFile from the egress capture pipe");
            buf.truncate(got as usize);
            buf
        }
    }

    #[cfg(windows)]
    impl Drop for CaptureSink {
        fn drop(&mut self) {
            // Wake the session's waiter (it is parked on the event we handed over),
            // then unregister: the last `Arc` drop closes the pipe's write end and
            // the session's copy of the event.
            // SAFETY: handles this struct owns, closed exactly once.
            unsafe { winapi::SetEvent(self.stop) };
            aterm_pty::close_master(self.master);
            unsafe {
                winapi::CloseHandle(self.stop);
                winapi::CloseHandle(self.read);
            }
        }
    }

    /// The kernel32 entry points the Windows capture needs. Hand-rolled
    /// `extern "system"` against the already-linked kernel32, matching the house
    /// style of `aterm_pty::windows::ffi` (flat C, no COM, no new dependency).
    #[cfg(windows)]
    mod winapi {
        // Win32 ABI names verbatim, so they can be checked against the SDK headers
        // line by line — the same rule `aterm_pty::windows::ffi` states.
        #![allow(non_snake_case)]

        use std::ffi::c_void;

        #[link(name = "kernel32")]
        unsafe extern "system" {
            pub fn CreatePipe(
                read: *mut isize,
                write: *mut isize,
                attrs: *mut c_void,
                size: u32,
            ) -> i32;
            pub fn CreateEventW(
                attrs: *mut c_void,
                manual_reset: i32,
                initial_state: i32,
                name: *const u16,
            ) -> isize;
            pub fn SetEvent(event: isize) -> i32;
            pub fn CloseHandle(handle: isize) -> i32;
            pub fn PeekNamedPipe(
                pipe: isize,
                buf: *mut u8,
                buf_len: u32,
                read: *mut u32,
                avail: *mut u32,
                left_this_message: *mut u32,
            ) -> i32;
            pub fn ReadFile(
                file: isize,
                buf: *mut u8,
                want: u32,
                got: *mut u32,
                overlapped: *mut c_void,
            ) -> i32;
        }
    }

    /// Drive ONE [`InputEvent`] through [`seam_egress`] against a real
    /// [`SinkWriter`] over a readable master, and return the exact bytes that
    /// reached the "PTY".
    fn egress_bytes(term: &Mutex<Terminal>, ev: &InputEvent) -> Vec<u8> {
        let mut cap = CaptureSink::new();
        let sink = SinkWriter::new(cap.master());
        seam_egress(term, &sink, ev, EgressMode::Interactive);
        drop(sink);
        cap.drain()
    }

    /// `delivered` classifies a `write_frame` result: a full write is `Full`; a
    /// short write (peer closed mid-frame, `Ok(n<intended)`) or a hard error is
    /// `Failed` — the property the false-OK fix rests on.
    #[test]
    fn delivered_classifies_short_and_failed_writes() {
        assert_eq!(delivered(Ok(5), 5), Delivery::Full);
        assert_eq!(delivered(Ok(0), 5), Delivery::Failed); // peer closed mid-frame
        assert_eq!(delivered(Ok(3), 5), Delivery::Failed); // short
        assert_eq!(
            delivered(Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe)), 5),
            Delivery::Failed
        );
    }

    /// REPLY FIDELITY (audit Finding 1): when the PTY write FAILS, the seam reports
    /// `Delivery::Failed` (→ `InputOutcome::WriteFailed` → `ERR write failed`), NEVER
    /// a false OK. An invalid fd makes every write fail deterministically (EBADF, so
    /// no SIGPIPE). This is the input-seam conformance to the reply-fidelity property
    /// class: OK iff the bytes actually landed. A faithful empty encoding (a legacy
    /// key RELEASE — nothing to write) stays `Full`: there was nothing to lose.
    #[test]
    fn failed_pty_write_is_reported_not_falsely_ok() {
        use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
        let term = term_with(&[]);
        let sink = SinkWriter::new(-1); // invalid fd -> every write_frame errors
        let press = InputEvent::Key {
            key: Key::Named(NamedKey::ArrowUp),
            mods: Modifiers::empty(),
            base_layout: None,
            event_type: KeyEventType::Press,
        };
        assert_eq!(
            seam_egress(&term, &sink, &press, EgressMode::Interactive),
            Egress::Reported(Delivery::Failed)
        );

        let release = InputEvent::Key {
            key: Key::Named(NamedKey::ArrowUp),
            mods: Modifiers::empty(),
            base_layout: None,
            event_type: KeyEventType::Release, // legacy: encodes to nothing
        };
        assert_eq!(
            seam_egress(&term, &sink, &release, EgressMode::Interactive),
            Egress::Reported(Delivery::Full)
        );
    }

    /// The `[key_sequences]` egress (`InputEvent::KeySequence`) writes the user's bytes
    /// to the PTY VERBATIM — no keyboard-mode read, no encoder — even under a mode that
    /// WOULD re-encode a real key. Empty bytes are a faithful no-op (`Full`), and a
    /// failed write is reported (`Failed`), never a false OK. (Guards against a future
    /// refactor "helpfully" routing KeySequence through the encoder.)
    #[test]
    fn key_sequence_egress_is_verbatim_and_reply_faithful() {
        // Kitty disambiguate + DECCKM: a real ArrowUp here would be re-encoded, but the
        // raw mapped bytes must pass through unchanged.
        let term = term_with(&[b"\x1b[>1u", b"\x1b[?1h"]);
        assert_eq!(
            egress_bytes(&term, &InputEvent::KeySequence(b"\x1b[15~".to_vec())),
            b"\x1b[15~",
            "mapped bytes are sent verbatim regardless of keyboard mode"
        );
        // Empty mapping writes nothing and is a faithful `Full` no-op (not a failure).
        let empty = InputEvent::KeySequence(Vec::new());
        assert_eq!(egress_bytes(&term, &empty), b"");
        assert_eq!(egress_of(&term, &empty), Egress::Reported(Delivery::Full));
        // A failed write is reported, not falsely OK.
        let sink = SinkWriter::new(-1);
        assert_eq!(
            seam_egress(
                &term,
                &sink,
                &InputEvent::KeySequence(b"hi".to_vec()),
                EgressMode::Interactive
            ),
            Egress::Reported(Delivery::Failed)
        );
    }

    /// An IME commit has no physical key identity. Under Kitty report-all,
    /// preserve it as one keyless event; never leak one decimal CSI-u key report
    /// per Unicode codepoint into the PTY.
    #[test]
    fn ime_commit_report_all_is_one_keyless_event_at_the_pty_seam() {
        let term = term_with(&[b"\x1b[>8u"]);
        assert_eq!(
            egress_bytes(&term, &InputEvent::Text("日本".to_string())),
            b"\x1b[0u"
        );

        let with_text = term_with(&[b"\x1b[>24u"]);
        assert_eq!(
            egress_bytes(&with_text, &InputEvent::Text("日本".to_string())),
            b"\x1b[0;1;26085:26412u"
        );
    }

    /// Compile-time witness: every `InputEvent` variant is enumerated here with NO
    /// wildcard arm, so adding a variant breaks THIS match and forces the author to also
    /// add a representative to the `events` matrix in `bytes_human_eq_controller` (a
    /// hand-maintained Vec with no such structural guard). Never called — it exists to
    /// fail to COMPILE if a variant is added, not to run. KeySequence is the variant
    /// this guard would have caught.
    #[allow(dead_code)]
    fn _convergence_matrix_is_exhaustive(ev: &InputEvent) {
        match ev {
            InputEvent::Key { .. }
            | InputEvent::Text(_)
            | InputEvent::KeySequence(_)
            | InputEvent::MouseButton { .. }
            | InputEvent::MouseMove { .. }
            | InputEvent::Wheel { .. }
            | InputEvent::ScrollView(_)
            | InputEvent::Paste(_)
            | InputEvent::Resize { .. }
            | InputEvent::ResizeWindowPx { .. }
            | InputEvent::Focus(_) => {}
        }
    }

    /// A `Terminal` with the given mode-enabling sequences fed in (DECCKM, Kitty,
    /// the mouse modes, focus reporting, bracketed paste).
    fn term_with(seqs: &[&[u8]]) -> Arc<Mutex<Terminal>> {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        {
            let mut t = term.lock().unwrap();
            for s in seqs {
                t.process(s);
            }
        }
        term
    }

    /// A wheel event at the cell origin (row 0, col 0), no modifiers.
    fn wheel(dir: WheelDir, lines: i32) -> InputEvent {
        InputEvent::Wheel {
            dir,
            lines,
            row: 0,
            col: 0,
            mods: 0,
            px_off: PixelOffset::CELL_ORIGIN,
        }
    }

    /// A wheel event carrying mouse modifier bits (see `App::mouse_modifiers`).
    fn wheel_mods(dir: WheelDir, lines: i32, mods: u8) -> InputEvent {
        InputEvent::Wheel {
            dir,
            lines,
            row: 0,
            col: 0,
            mods,
            px_off: PixelOffset::CELL_ORIGIN,
        }
    }

    /// Drive ONE event through [`seam_egress`] and return the [`Egress`] verdict
    /// (the variant the viewport side-effects key off), discarding any PTY bytes.
    fn egress_of(term: &Mutex<Terminal>, ev: &InputEvent) -> Egress {
        let cap = CaptureSink::new();
        let sink = SinkWriter::new(cap.master());
        seam_egress(term, &sink, ev, EgressMode::Interactive)
    }

    /// SELECTION CUSTODY Phase 2 — the LOCAL-SCROLL OVERRIDE, on EVERY platform.
    ///
    /// The two `Egress`-level tests below say the same thing through the real seam
    /// (and now do so on every platform — see [`CaptureSink`]). This one goes
    /// through [`wheel_route_for`] — the engine reads plus the policy table, no
    /// sink at all — so the override the design calls load-bearing ("without this,
    /// Phase 4 is unreachable under any mouse-owning TUI") stays pinned even for a
    /// target with no PTY of any kind, and fails at the policy table rather than at
    /// a byte string when it regresses.
    #[test]
    fn the_option_wheel_override_routes_locally_on_every_platform() {
        use aterm_types::mouse::{ALT_MASK, SHIFT_MASK};

        let route = |term: &Mutex<Terminal>, mods: u8| {
            let t = term.lock().expect("terminal");
            super::wheel_route_for(&t, mods)
        };

        // MAIN screen, app grabbing the mouse (SGR tracking).
        let tracking = term_with(&[b"\x1b[?1000h", b"\x1b[?1006h"]);
        assert_eq!(
            route(&tracking, 0),
            WheelRoute::Report,
            "control: a tracking app still owns the plain wheel"
        );
        assert_eq!(
            route(&tracking, ALT_MASK),
            WheelRoute::Viewport,
            "Option+wheel must reach the local scrollback under tracking"
        );
        assert_eq!(
            route(&tracking, SHIFT_MASK),
            WheelRoute::Viewport,
            "…and the older Shift bypass is untouched"
        );

        // ALT screen: the override is scoped OUT. There is no scrollback to reach
        // there, and stealing the wheel would cost the DEC-1007 arrows that
        // less/man/git-log scroll by.
        let alt_tracking = term_with(&[b"\x1b[?1049h", b"\x1b[?1000h", b"\x1b[?1006h"]);
        assert_eq!(
            route(&alt_tracking, ALT_MASK),
            WheelRoute::Report,
            "a mouse-owning alt-screen app keeps the wheel even with Option held"
        );
        let alt_scroll = term_with(&[b"\x1b[?1007h", b"\x1b[?1049h"]);
        assert_eq!(
            route(&alt_scroll, ALT_MASK),
            WheelRoute::AltScroll,
            "alternate scroll still converts the wheel to an arrow"
        );

        // No tracking, no alt screen: the wheel was always local, with or without
        // Option — so the override adds a path, it does not redirect one.
        let plain = term_with(&[]);
        assert_eq!(route(&plain, 0), WheelRoute::Viewport);
        assert_eq!(route(&plain, ALT_MASK), WheelRoute::Viewport);
    }

    /// SELECTION CUSTODY Phase 2 — the LOCAL-SCROLL OVERRIDE.
    ///
    /// A program that grabs the mouse takes the wheel absolutely, so while it runs
    /// the user cannot reach their own scrollback at all. With Option held the wheel
    /// scrolls this terminal instead — the wheel half of the gesture
    /// `press_starts_selection` already gives selection under tracking.
    #[test]
    fn option_wheel_scrolls_locally_under_mouse_tracking() {
        use aterm_types::mouse::ALT_MASK;
        // Main screen, app grabbing the mouse (SGR tracking).
        let term = term_with(&[b"\x1b[?1000h", b"\x1b[?1006h"]);
        // Control: without the override the app owns the wheel.
        assert!(
            matches!(
                egress_of(&term, &wheel(WheelDir::Up, 1)),
                Egress::Reported(_)
            ),
            "a tracking app still gets the plain wheel"
        );
        // With Option held the local viewport scrolls instead.
        assert!(
            matches!(
                egress_of(&term, &wheel_mods(WheelDir::Up, 1, ALT_MASK)),
                Egress::TrackingOff { .. }
            ),
            "Option+wheel must reach the local scrollback"
        );
    }

    /// …and it is scoped to the MAIN screen. The alt screen is built with zero
    /// scrollback, so an override there would reach nothing WHILE costing the
    /// alternate-scroll arrows that are how `less`/`man`/`git log` scroll.
    ///
    /// STILL `#[cfg(unix)]`, and not for want of a harness (the byte capture is
    /// cross-platform now — see [`CaptureSink`]): the assertion is `== b"\x1b[A"`,
    /// ONE arrow for one line, which is true only where `wheel_platform_lines` is
    /// the identity. On Windows the same gesture is worth `SPI_GETWHEELSCROLLLINES`
    /// arrows — a number that belongs to whoever's machine runs the suite and so
    /// can never be asserted. The Windows side of that multiply has its own test,
    /// `wheel_scale_is_the_platform_distance_not_one_line`, and the ROUTE half of
    /// this test (Option must not steal a tracking app's report) is pinned on every
    /// platform by `the_option_wheel_override_routes_locally_on_every_platform`.
    #[cfg(unix)]
    #[test]
    fn the_local_scroll_override_leaves_the_alt_screen_alone() {
        use aterm_types::mouse::ALT_MASK;
        // Alt screen + DEC 1007: Option must NOT steal the arrows.
        let alt_scroll = term_with(&[b"\x1b[?1007h", b"\x1b[?1049h"]);
        assert_eq!(
            egress_bytes(&alt_scroll, &wheel_mods(WheelDir::Up, 1, ALT_MASK)),
            b"\x1b[A",
            "alternate scroll still converts the wheel to an arrow"
        );
        // Alt screen + a tracking app: Option must NOT steal the report either.
        let tracking = term_with(&[b"\x1b[?1049h", b"\x1b[?1000h", b"\x1b[?1006h"]);
        assert!(
            matches!(
                egress_of(&tracking, &wheel_mods(WheelDir::Up, 1, ALT_MASK)),
                Egress::Reported(_)
            ),
            "a mouse-owning alt-screen app keeps the wheel"
        );
    }

    /// Audit M5 — alternate scroll (DEC 1007): on the ALT screen with 1007 set and
    /// no mouse tracking, a wheel becomes arrow-key PRESSES (one per line), encoded
    /// through the LIVE keyboard mode (SS3 under DECCKM), and the local viewport is
    /// left alone (`Egress::Reported`, not `TrackingOff`).
    ///
    /// STILL `#[cfg(unix)]`: every assertion here counts ARROWS (`2` lines → two
    /// `ESC[A`), and Windows multiplies that count by the live
    /// `SPI_GETWHEELSCROLLLINES` — an unassertable machine setting. See
    /// `wheel_scale_is_the_platform_distance_not_one_line` for the Windows half.
    #[cfg(unix)]
    #[test]
    fn alt_screen_alternate_scroll_converts_wheel_to_arrows() {
        let term = term_with(&[b"\x1b[?1007h", b"\x1b[?1049h"]);
        assert_eq!(egress_bytes(&term, &wheel(WheelDir::Up, 2)), b"\x1b[A\x1b[A");
        assert_eq!(egress_bytes(&term, &wheel(WheelDir::Down, 1)), b"\x1b[B");
        // DECCKM (?1h): arrows switch to the SS3 form, proving the bytes come from
        // the live keyboard mode, not a hardcoded CSI.
        term.lock().unwrap().process(b"\x1b[?1h");
        assert_eq!(egress_bytes(&term, &wheel(WheelDir::Up, 1)), b"\x1bOA");
        // The wheel is consumed as a key report: the viewport must NOT scroll.
        assert!(matches!(
            egress_of(&term, &wheel(WheelDir::Up, 1)),
            Egress::Reported(_)
        ));
    }

    /// Mouse tracking takes precedence: a tracking app gets a real SGR wheel report,
    /// never a synthesized arrow.
    #[test]
    fn alt_screen_mouse_tracking_beats_alternate_scroll() {
        let term = term_with(&[
            b"\x1b[?1049h",
            b"\x1b[?1007h",
            b"\x1b[?1000h",
            b"\x1b[?1006h",
        ]);
        assert_eq!(egress_bytes(&term, &wheel(WheelDir::Up, 1)), b"\x1b[<64;1;1M");
    }

    /// I12 at the byte level: the SAME tracking + alt-scroll terminal as above,
    /// but with SHIFT on the event — the seam falls back to the local viewport
    /// (`TrackingOff` carrying the gesture's lines) and writes NOTHING, so a
    /// shifted report byte (`64|SHIFT_MASK`) can never leak to the app.
    #[test]
    fn shift_wheel_bypasses_a_tracking_app_with_no_report_bytes() {
        let term = term_with(&[
            b"\x1b[?1049h",
            b"\x1b[?1007h",
            b"\x1b[?1000h",
            b"\x1b[?1006h",
        ]);
        let shifted = InputEvent::Wheel {
            dir: WheelDir::Up,
            lines: 2,
            row: 0,
            col: 0,
            mods: aterm_types::mouse::SHIFT_MASK,
            px_off: PixelOffset::CELL_ORIGIN,
        };
        assert!(matches!(
            egress_of(&term, &shifted),
            Egress::TrackingOff {
                wheel_lines: 2,
                wheel_up: true
            }
        ));
        assert!(egress_bytes(&term, &shifted).is_empty());
        // Control: the unshifted twin still reports (the bypass is the ONLY change).
        assert_eq!(egress_bytes(&term, &wheel(WheelDir::Up, 1)), b"\x1b[<64;1;1M");
    }

    /// Alternate scroll applies only on the ALT screen: on the main screen the wheel
    /// falls back to local scrollback (`Egress::TrackingOff`), no arrows synthesized.
    #[test]
    fn alternate_scroll_only_on_the_alt_screen() {
        let term = term_with(&[b"\x1b[?1007h"]);
        assert!(matches!(
            egress_of(&term, &wheel(WheelDir::Up, 1)),
            Egress::TrackingOff {
                wheel_lines: 1,
                wheel_up: true
            }
        ));
        assert!(egress_bytes(&term, &wheel(WheelDir::Up, 1)).is_empty());
    }

    /// Audit I7, THE WHOLE POINT, and cross-platform (no `cfg(unix)` — it asserts
    /// the ROUTE, which needs no pipe): a horizontal wheel is REPORTED while an app
    /// tracks the mouse, and produces ZERO local motion when nothing does.
    ///
    /// The tracking-OFF half is the regression fence for the guard this widening
    /// replaced: `wheel_lines: 0` is the seam telling `input_wheel` "there is
    /// nothing to scroll", so a tilt notch cannot become the phantom scroll-DOWN
    /// the original early-return was added to kill.
    #[test]
    fn horizontal_wheel_reports_only_while_tracking() {
        let sink = SinkWriter::new(-1);
        for dir in [WheelDir::Left, WheelDir::Right] {
            let tracking = term_with(&[b"\x1b[?1000h", b"\x1b[?1006h"]);
            assert!(
                matches!(
                    seam_egress(&tracking, &sink, &wheel(dir, 2), EgressMode::Interactive),
                    Egress::Reported(_)
                ),
                "{dir:?} must reach a tracking app"
            );
            let idle = term_with(&[]);
            assert_eq!(
                seam_egress(&idle, &sink, &wheel(dir, 2), EgressMode::Interactive),
                Egress::TrackingOff {
                    wheel_lines: 0,
                    wheel_up: false
                },
                "{dir:?} must move nothing locally"
            );
            // …and the same with the alt screen + DEC 1007 armed: alternate scroll
            // is a VERTICAL substitute (arrows), never a horizontal one.
            let pager = term_with(&[b"\x1b[?1049h", b"\x1b[?1007h"]);
            assert_eq!(
                seam_egress(&pager, &sink, &wheel(dir, 2), EgressMode::Interactive),
                Egress::TrackingOff {
                    wheel_lines: 0,
                    wheel_up: false
                },
                "{dir:?} must not synthesize arrows for a pager"
            );
            // …and under the I12 Shift bypass, which asks aterm to take the
            // gesture: aterm has no horizontal viewport, so it takes nothing.
            let shifted = InputEvent::Wheel {
                dir,
                lines: 2,
                row: 0,
                col: 0,
                mods: aterm_types::mouse::SHIFT_MASK,
                px_off: PixelOffset::CELL_ORIGIN,
            };
            let tracking = term_with(&[b"\x1b[?1000h", b"\x1b[?1006h"]);
            assert_eq!(
                seam_egress(&tracking, &sink, &shifted, EgressMode::Interactive),
                Egress::TrackingOff {
                    wheel_lines: 0,
                    wheel_up: false
                },
                "shift+{dir:?} bypasses to a viewport that cannot pan"
            );
        }
        // CONTROL: the vertical twin is untouched — it still carries its lines to
        // the viewport, which is the behaviour this change must not disturb.
        let idle = term_with(&[]);
        assert_eq!(
            seam_egress(
                &idle,
                &sink,
                &wheel(WheelDir::Up, 2),
                EgressMode::Interactive
            ),
            Egress::TrackingOff {
                wheel_lines: 2,
                wheel_up: true
            }
        );
    }

    /// An app can turn alternate scroll OFF (?1007l): the wheel then falls back to
    /// the local scrollback viewport even on the alt screen.
    #[test]
    fn alt_screen_with_1007_off_falls_back_to_viewport() {
        let term = term_with(&[b"\x1b[?1049h", b"\x1b[?1007l"]);
        assert!(matches!(
            egress_of(&term, &wheel(WheelDir::Up, 1)),
            Egress::TrackingOff { .. }
        ));
        assert!(egress_bytes(&term, &wheel(WheelDir::Up, 1)).is_empty());
    }

    /// One arrow press is emitted PER accumulated wheel line — on a platform whose
    /// lines-per-detent is the identity ([`wheel_platform_lines`]). On Windows the
    /// same gesture is worth `SPI_GETWHEELSCROLLLINES` arrows; see
    /// `wheel_scale_is_the_platform_distance_not_one_line`.
    ///
    /// STILL `#[cfg(unix)]` for exactly the reason its own name states — the
    /// harness is cross-platform now ([`CaptureSink`]), the ASSERTION is not.
    #[cfg(unix)]
    #[test]
    fn alternate_scroll_emits_one_arrow_per_line() {
        let term = term_with(&[b"\x1b[?1049h", b"\x1b[?1007h"]);
        assert_eq!(egress_bytes(&term, &wheel(WheelDir::Up, 3)), b"\x1b[A\x1b[A\x1b[A");
    }

    /// The report burst is bounded at BOTH ends, for every source. A tracking app
    /// gets exactly one report per line up to [`MAX_WHEEL_BURST`], and a
    /// non-positive count (a future grammar bug) is floored at one rather than
    /// silently emitting nothing for one source and N for another.
    ///
    /// The ceiling used to live only in the `mouse` verb's `lines=N` parse, so a
    /// CONTROLLER was bounded and a human gesture was not — a divergence the seam
    /// exists to make impossible, and live exposure once the horizontal axis
    /// started dividing trackpad pixel deltas by the (small) cell WIDTH.
    #[test]
    fn the_report_burst_is_clamped_at_both_ends_for_every_source() {
        let term = term_with(&[b"\x1b[?1000h", b"\x1b[?1006h"]);
        let one = b"\x1b[<64;1;1M";
        // One report per line, unscaled, below the ceiling.
        assert_eq!(
            egress_bytes(&term, &wheel(WheelDir::Up, 3)).len(),
            one.len() * 3
        );
        // At the ceiling, and clamped just above it. Deliberately only just
        // above: `egress_bytes` reads its pipe AFTER the write, so an unclamped
        // burst big enough to exceed the pipe buffer would DEADLOCK the test
        // instead of failing it, and a hang is a much worse failure signal.
        assert_eq!(
            egress_bytes(&term, &wheel(WheelDir::Up, MAX_WHEEL_BURST)).len(),
            one.len() * MAX_WHEEL_BURST as usize
        );
        assert_eq!(
            egress_bytes(&term, &wheel(WheelDir::Up, MAX_WHEEL_BURST + 3)).len(),
            one.len() * MAX_WHEEL_BURST as usize,
            "one event cannot flood the PTY past the ceiling"
        );
        assert_eq!(
            egress_bytes(&term, &wheel(WheelDir::Left, MAX_WHEEL_BURST + 3)).len(),
            one.len() * MAX_WHEEL_BURST as usize,
            "the horizontal axis takes the same ceiling"
        );
        // The floor: zero and negative counts still emit exactly one report.
        assert_eq!(egress_bytes(&term, &wheel(WheelDir::Up, 0)), one);
        assert_eq!(egress_bytes(&term, &wheel(WheelDir::Up, -7)), one);
    }

    /// THE WHEEL MULTIPLY ITSELF (Windows). The live
    /// `SPI_GETWHEELSCROLLLINES` read cannot be asserted — its answer belongs to
    /// whoever's machine runs the suite — so the arithmetic every wheel gesture
    /// goes through is pinned here instead, on all three of the setting's shapes.
    /// Both surfaces that scroll without an app's help route through this: the
    /// scrollback viewport (`App::input_wheel`) and the DEC-1007 alt-scroll arrows.
    #[cfg(windows)]
    #[test]
    fn wheel_scale_is_the_platform_distance_not_one_line() {
        use crate::platform_win::WheelNotch;
        // The Windows default, and the whole point: one detent is THREE lines, not
        // the one that winit's `LineDelta` literally reports.
        assert_eq!(wheel_scaled_lines(1, WheelNotch::Lines(3), 24), 3);
        assert_eq!(wheel_scaled_lines(4, WheelNotch::Lines(3), 24), 12);
        // A user who set the slider to 1 keeps the old behaviour exactly.
        assert_eq!(wheel_scaled_lines(2, WheelNotch::Lines(1), 24), 2);
        // "One screen at a time" is a PAGE, sized as the viewport so a wheel and
        // PgUp/PgDn cannot disagree; a degenerate zero-row grid still moves.
        assert_eq!(wheel_scaled_lines(1, WheelNotch::Page, 24), 24);
        assert_eq!(wheel_scaled_lines(2, WheelNotch::Page, 50), 100);
        assert_eq!(wheel_scaled_lines(1, WheelNotch::Page, 0), 1);
        // "Wheel scrolling off" (a literal 0) is honoured as NO motion rather than
        // clamped up to one line: a user who turned the wheel off meant it.
        assert_eq!(wheel_scaled_lines(9, WheelNotch::Lines(0), 24), 0);
        // Absurd values saturate instead of wrapping into a negative scroll.
        assert_eq!(
            wheel_scaled_lines(i32::MAX, WheelNotch::Lines(u32::MAX), 24),
            i32::MAX
        );
    }

    /// THE Tier-1 indistinguishability invariant (A.7), part 1 — BYTE EQUALITY.
    ///
    /// For the SAME logical `InputEvent`, the bytes a Human source and a Controller
    /// source put on the wire are BYTE-IDENTICAL — across a matrix of keyboard
    /// modes x mouse-tracking modes x event kinds. `seam_egress` takes no `Source`
    /// (so this is enforced by construction); the assertion drives the SAME core
    /// both sources reach via `App::input`. The `Buggy` negative control below
    /// proves the assertion has teeth, and `builders_converge` (part 2) proves the
    /// two REAL builders feed `seam_egress` structurally-equal events for the same
    /// intent — so the chain Human-builder → seam == Controller-builder → seam is
    /// complete, not tautological.
    #[test]
    fn bytes_human_eq_controller() {
        use aterm_types::keyboard::{Key, Modifiers, NamedKey};
        use aterm_types::mouse::{CTRL_MASK, MouseButton, SHIFT_MASK};

        // Keyboard modes: legacy, DECCKM (app cursor keys), Kitty disambiguate +
        // REPORT_ALTERNATE_KEYS (proves base_layout flows identically — divergence
        // h). Mouse modes: off, Normal(1000), Button(1002), Any(1003) + SGR(1006).
        let kbd_modes: &[&[&[u8]]] = &[
            &[],            // legacy
            &[b"\x1b[?1h"], // DECCKM
            &[b"\x1b[>1u"], // Kitty disambiguate
            &[b"\x1b[>5u"], // Kitty disambiguate + report-alternate
        ];
        let mouse_modes: &[&[&[u8]]] = &[
            &[],                               // tracking off
            &[b"\x1b[?1000h", b"\x1b[?1006h"], // Normal + SGR
            &[b"\x1b[?1002h", b"\x1b[?1006h"], // ButtonEvent + SGR
            &[b"\x1b[?1003h", b"\x1b[?1006h"], // AnyEvent + SGR
        ];

        let events = vec![
            InputEvent::Key {
                key: Key::Character('a'),
                mods: Modifiers::CTRL | Modifiers::SHIFT,
                base_layout: Some('a'),
                event_type: aterm_types::keyboard::KeyEventType::Press,
            },
            InputEvent::Key {
                key: Key::Named(NamedKey::ArrowUp),
                mods: Modifiers::empty(),
                base_layout: None,
                event_type: aterm_types::keyboard::KeyEventType::Press,
            },
            InputEvent::Key {
                key: Key::Named(NamedKey::Enter),
                mods: Modifiers::empty(),
                base_layout: None,
                event_type: aterm_types::keyboard::KeyEventType::Press,
            },
            InputEvent::Text("héllo 日本".to_string()),
            InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: true,
                row: 5,
                col: 9,
                mods: SHIFT_MASK | CTRL_MASK,
                click_count: 2,
                side: SelectionSide::Right,
                block: true,
                suppress_copy_on_select: false,
                px_off: PixelOffset { x: 3, y: 7 },
            },
            InputEvent::MouseButton {
                button: MouseButton::Right,
                pressed: false,
                row: 5,
                col: 9,
                mods: 0,
                click_count: 1,
                side: SelectionSide::Left,
                block: false,
                suppress_copy_on_select: false,
                px_off: PixelOffset::CELL_ORIGIN,
            },
            InputEvent::MouseMove {
                buttons: 0,
                row: 7,
                col: 3,
                mods: 0,
                side: SelectionSide::Left,
                px_off: PixelOffset { x: 1, y: 2 },
            },
            InputEvent::MouseMove {
                buttons: 3,
                row: 7,
                col: 3,
                mods: 0,
                side: SelectionSide::Left,
                px_off: PixelOffset::CELL_ORIGIN,
            },
            InputEvent::Wheel {
                dir: WheelDir::Up,
                lines: 3,
                row: 2,
                col: 4,
                mods: 0,
                px_off: PixelOffset::CELL_ORIGIN,
            },
            InputEvent::Wheel {
                dir: WheelDir::Down,
                lines: 1,
                row: 2,
                col: 4,
                mods: 0,
                px_off: PixelOffset::CELL_ORIGIN,
            },
            // The HORIZONTAL axis (audit I7) is in the matrix for the same
            // reason the vertical pair is: it produces PTY bytes under a
            // tracking app, so a human tilt and a `mouse wheelleft` verb must
            // agree byte-for-byte in every mouse mode — including the
            // tracking-off column, where BOTH must write nothing.
            InputEvent::Wheel {
                dir: WheelDir::Left,
                lines: 2,
                row: 2,
                col: 4,
                mods: 0,
                px_off: PixelOffset::CELL_ORIGIN,
            },
            InputEvent::Wheel {
                dir: WheelDir::Right,
                lines: 1,
                row: 2,
                col: 4,
                mods: 0,
                px_off: PixelOffset::CELL_ORIGIN,
            },
            // The THUMB pair (audit I8): xterm buttons 8/9, whose Cb base of
            // 128 exercises a byte range no other event in this matrix reaches
            // (the UTF-8 encoding's two-byte button code, and the legacy
            // release substitution on a high button).
            InputEvent::MouseButton {
                button: MouseButton::Back,
                pressed: true,
                row: 5,
                col: 9,
                mods: 0,
                click_count: 1,
                side: SelectionSide::Left,
                block: false,
                suppress_copy_on_select: false,
                px_off: PixelOffset::CELL_ORIGIN,
            },
            InputEvent::MouseButton {
                button: MouseButton::Forward,
                pressed: false,
                row: 5,
                col: 9,
                mods: 0,
                click_count: 1,
                side: SelectionSide::Left,
                block: false,
                suppress_copy_on_select: false,
                px_off: PixelOffset::CELL_ORIGIN,
            },
            InputEvent::Paste("rm -rf safe".to_string()),
            InputEvent::Focus(true),
            InputEvent::Focus(false),
            // [key_sequences] override: written to the PTY verbatim, no Source and no
            // mode read, so Human==Controller is trivially true — which is exactly the
            // source-blind property worth pinning. Empty + non-empty both covered.
            InputEvent::KeySequence(b"\x1b[15~".to_vec()),
            InputEvent::KeySequence(Vec::new()),
        ];

        for kbd in kbd_modes {
            for mouse in mouse_modes {
                // Two INDEPENDENT terminals in the identical mode — one stands in for
                // the human-driven session, one for the controller-driven session.
                let mut seqs: Vec<&[u8]> = Vec::new();
                seqs.extend_from_slice(kbd);
                seqs.extend_from_slice(mouse);
                seqs.push(b"\x1b[?1004h"); // focus reporting on
                seqs.push(b"\x1b[?2004h"); // bracketed paste on
                let term_human = term_with(&seqs);
                let term_ctrl = term_with(&seqs);

                for ev in &events {
                    let human = egress_bytes(&term_human, ev);
                    let controller = egress_bytes(&term_ctrl, ev);
                    assert_eq!(
                        human, controller,
                        "bytes(Human) != bytes(Controller) for {ev:?} under kbd={kbd:?} mouse={mouse:?}"
                    );
                }
            }
        }
    }

    /// THE Tier-1 indistinguishability invariant (A.7), part 2 — BUILDER EQUALITY.
    ///
    /// The byte test (part 1) feeds one event to two terminals; this proves the
    /// event a Human builds equals the event a Controller builds for the same
    /// intent — closing the "but the builders could diverge" gap. We can't
    /// construct a winit `KeyEvent` (its `platform_specific` field is `pub(crate)`),
    /// so we drive the SAME primitives `keymap::build_key_input` uses
    /// (`aterm_winit_keymap::{map_logical_key, base_layout_key_for}`) as the
    /// human side, and the real control-verb parsers (`control::parse_key`,
    /// `parse_ctrl`, `parse_mouse`) as the controller side. For the named-key /
    /// ctrl-chord intents both sides land on the identical `InputEvent` — and then
    /// `seam_egress` gives identical bytes.
    #[test]
    fn builders_converge() {
        use aterm_types::keyboard::{Key, Modifiers, NamedKey};
        use aterm_winit_keymap::{base_layout_key_for, map_logical_key};
        use winit::keyboard::{Key as WinitKey, KeyCode, NamedKey as WinitNamed, PhysicalKey};

        // --- "press the Up arrow" --------------------------------------------
        // Human: build_key_input's pure decision on a winit ArrowUp event.
        let human_up = {
            let key = map_logical_key(&WinitKey::Named(WinitNamed::ArrowUp)).expect("up maps");
            let base = base_layout_key_for(PhysicalKey::Code(KeyCode::ArrowUp));
            InputEvent::Key {
                key,
                mods: Modifiers::empty(),
                base_layout: base,
                event_type: aterm_types::keyboard::KeyEventType::Press,
            }
        };
        // Controller: the real `key up` parser.
        let ctrl_up = crate::control::parse_key("up").expect("key up parses");
        assert_eq!(
            human_up, ctrl_up,
            "human `Up` builder != controller `key up` builder"
        );
        assert_eq!(
            human_up,
            InputEvent::Key {
                key: Key::Named(NamedKey::ArrowUp),
                mods: Modifiers::empty(),
                base_layout: None,
                event_type: aterm_types::keyboard::KeyEventType::Press
            },
        );

        // --- "Ctrl+C" --------------------------------------------------------
        // Human: build_key_input's decision on a winit 'c' event with CTRL.
        let human_ctrl_c = {
            let key = map_logical_key(&WinitKey::Character("c".into())).expect("c maps");
            let base = base_layout_key_for(PhysicalKey::Code(KeyCode::KeyC));
            InputEvent::Key {
                key,
                mods: Modifiers::CTRL,
                base_layout: base,
                event_type: aterm_types::keyboard::KeyEventType::Press,
            }
        };
        let ctrl_ctrl_c = crate::control::parse_ctrl("c").expect("ctrl c parses");
        // Both encode Ctrl+c; the human path carries the physical base_layout, the
        // controller carries None. They must produce IDENTICAL BYTES (base_layout
        // only adds the Kitty 3rd field, which a plain `c` does not change), so we
        // assert byte-equality through the seam, not struct-equality, here.
        for seqs in [&[][..], &[&b"\x1b[>1u"[..]][..]] {
            let term = term_with(seqs);
            assert_eq!(
                egress_bytes(&term, &human_ctrl_c),
                egress_bytes(&term, &ctrl_ctrl_c),
                "Ctrl+C bytes diverge (human base_layout vs controller None) under {seqs:?}",
            );
        }

        // --- "mouse press, shift, double-click, right side, block" -----------
        // Controller: the real `mouse press` parser with the full additive grammar.
        let ctrl_press =
            crate::control::parse_mouse("press left 5 9 mods=shift count=2 side=right block=1")
                .expect("mouse press parses");
        // Human-equivalent: the on_mouse_input builder fields. mods=SHIFT_MASK,
        // count=2 from the streak FSM, side=Right (cell half), block from alt held.
        let human_press = InputEvent::MouseButton {
            button: aterm_types::mouse::MouseButton::Left,
            pressed: true,
            row: 5,
            col: 9,
            mods: aterm_types::mouse::SHIFT_MASK,
            click_count: 2,
            side: SelectionSide::Right,
            block: true,
            // parse_mouse itself (scope-blind) never sets suppression — the
            // scoped-edge policy is applied by cmd_mouse AFTER parsing — so the
            // pure parser output stays struct-identical to the human builder.
            suppress_copy_on_select: false,
            // The control verb carries no real pointer, so it (and the human
            // builder, for this struct-equality check) uses the cell origin.
            px_off: PixelOffset::CELL_ORIGIN,
        };
        assert_eq!(human_press, ctrl_press, "mouse-press builder mismatch");

        // --- "wheel up, 1 notch" ---------------------------------------------
        let ctrl_wheel = crate::control::parse_mouse("wheelup left 2 4").expect("wheelup parses");
        let human_wheel = InputEvent::Wheel {
            dir: WheelDir::Up,
            lines: 1,
            row: 2,
            col: 4,
            mods: 0,
            px_off: PixelOffset::CELL_ORIGIN,
        };
        assert_eq!(human_wheel, ctrl_wheel, "wheel builder mismatch");
    }

    /// NEGATIVE CONTROL: a `Buggy` egress that BRANCHES on the source (the exact
    /// thing the invariant forbids) MUST produce a counterexample — otherwise the
    /// byte test would pass even if someone reintroduced a source branch. Here the
    /// buggy variant drops the modifier bits for the controller, so a Ctrl+Shift
    /// chord diverges. This proves the test has teeth.
    #[test]
    fn buggy_source_branch_is_detectable() {
        use aterm_types::keyboard::{Key, Modifiers};

        fn buggy_key_bytes(
            term: &Mutex<Terminal>,
            ev: &InputEvent,
            is_controller: bool,
        ) -> Vec<u8> {
            let InputEvent::Key {
                key,
                mods,
                base_layout,
                event_type,
            } = ev
            else {
                return Vec::new();
            };
            // THE BUG: a behavioural branch on the source.
            let mods = if is_controller {
                Modifiers::empty()
            } else {
                *mods
            };
            let t = term_lock(term);
            aterm_types::keyboard::encode_key_with_layout(
                key,
                mods,
                t.keyboard_mode(),
                *event_type,
                *base_layout,
            )
        }

        let term = term_with(&[b"\x1b[>1u"]); // Kitty so modifiers are visible in CSI-u
        let ev = InputEvent::Key {
            key: Key::Character('a'),
            mods: Modifiers::CTRL | Modifiers::SHIFT,
            base_layout: Some('a'),
            event_type: aterm_types::keyboard::KeyEventType::Press,
        };
        let human = buggy_key_bytes(&term, &ev, false);
        let controller = buggy_key_bytes(&term, &ev, true);
        assert_ne!(
            human, controller,
            "the Buggy source-branch must be detectable (differing bytes), or the \
             indistinguishability test has no teeth"
        );
        // And the CORRECT, source-blind path agrees for the same event.
        assert_eq!(
            egress_bytes(&term, &ev),
            human,
            "source-blind == human path"
        );
    }

    /// The wheel-line clamp guards the human/controller asymmetry the critique
    /// flagged: a non-positive `lines` must NOT silently emit zero reports while a
    /// positive one emits N. With tracking ON, `lines: 0` and `lines: -3` both
    /// behave as exactly ONE report (the clamp to >= 1), identical to `lines: 1`.
    #[test]
    fn wheel_lines_clamped_to_one() {
        let term = term_with(&[b"\x1b[?1000h", b"\x1b[?1006h"]); // Normal + SGR tracking
        let one = egress_bytes(
            &term,
            &InputEvent::Wheel {
                dir: WheelDir::Up,
                lines: 1,
                row: 2,
                col: 4,
                mods: 0,
                px_off: PixelOffset::CELL_ORIGIN,
            },
        );
        for bad in [0, -1, -3] {
            let got = egress_bytes(
                &term,
                &InputEvent::Wheel {
                    dir: WheelDir::Up,
                    lines: bad,
                    row: 2,
                    col: 4,
                    mods: 0,
                    px_off: PixelOffset::CELL_ORIGIN,
                },
            );
            assert_eq!(
                got, one,
                "wheel lines={bad} must clamp to exactly one report"
            );
        }
    }

    /// DEC 1016 (SGR-PIXEL) mouse mode reports a GENUINE PIXEL coordinate, not the
    /// cell origin. With a 10×20 cell and a press in cell (col=3,row=2) at the
    /// sub-cell pixel offset (x=4,y=7), the reported pixel is `col*cw+x = 34`,
    /// `row*ch+y = 47`; `encode_sgr` adds the spec's +1, so the bytes are
    /// `ESC [ < 0 ; 35 ; 48 M`. This is the whole point of the lane: the report
    /// carries the real winit sub-cell pixel, not a cell-derived one.
    #[test]
    fn sgr_pixel_1016_reports_true_pixel_coordinate() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        {
            let mut t = term.lock().unwrap();
            t.set_cell_pixel_size(10, 20); // real font metrics the frontend reports
            t.process(b"\x1b[?1000h"); // Normal mouse tracking ON
            t.process(b"\x1b[?1016h"); // SGR-pixel encoding (DEC 1016)
        }
        let press = InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
            row: 2,
            col: 3,
            mods: 0,
            click_count: 1,
            side: SelectionSide::Left,
            block: false,
            suppress_copy_on_select: false,
            px_off: PixelOffset { x: 4, y: 7 },
        };
        assert_eq!(egress_bytes(&term, &press), b"\x1b[<0;35;48M");

        // The SAME logical press in plain SGR (1006, cell coords) reports the CELL
        // (col+1, row+1) — proving the pixel math is gated on 1016, not always on.
        let term_cell = term_with(&[b"\x1b[?1000h", b"\x1b[?1006h"]);
        assert_eq!(egress_bytes(&term_cell, &press), b"\x1b[<0;4;3M");
    }

    /// A 1016 report with the cell-origin offset (`CELL_ORIGIN`, the value a
    /// Controller sends) lands on the cell's top-left pixel: col=3,row=2 → pixel
    /// (30, 40) → `ESC [ < 0 ; 31 ; 41 M`. So a controller-driven 1016 press is
    /// still pixel-correct (the cell origin), and the sub-cell offset is purely
    /// additive on top.
    #[test]
    fn sgr_pixel_1016_cell_origin_is_top_left_pixel() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        {
            let mut t = term.lock().unwrap();
            t.set_cell_pixel_size(10, 20);
            t.process(b"\x1b[?1000h");
            t.process(b"\x1b[?1016h");
        }
        let press = InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
            row: 2,
            col: 3,
            mods: 0,
            click_count: 1,
            side: SelectionSide::Left,
            block: false,
            suppress_copy_on_select: false,
            px_off: PixelOffset::CELL_ORIGIN,
        };
        assert_eq!(egress_bytes(&term, &press), b"\x1b[<0;31;41M");
    }

    /// Jump-to-prompt must navigate by PROMPTS, not by finished commands.
    ///
    /// `command_marks()` is filled only by OSC 133;D, so a shell that marks
    /// where its prompt is but has no hook for "the command started executing"
    /// — cmd.exe, whose `%PROMPT%` can emit A and B and nothing else — recorded
    /// prompts that Ctrl+Shift+Up/Down could not reach. Measured before the fix
    /// on a real cmd tab: four prompt marks at rows 3/50/97/100 and
    /// `scroll prev-prompt` reporting `OK 0 71` at every step (never moved).
    #[test]
    fn jump_prompt_target_finds_prompts_from_an_a_b_only_shell() {
        let mut t = Terminal::new(4, 20);
        // Two A/B-only prompts separated by enough output to build scrollback,
        // exactly the shape cmd.exe's PROMPT produces.
        t.process(b"\x1b]133;A\x1b\\p1>\x1b]133;B\x1b\\\r\n");
        for _ in 0..6 {
            t.process(b"out\r\n");
        }
        t.process(b"\x1b]133;A\x1b\\p2>\x1b]133;B\x1b\\\r\n");
        for _ in 0..6 {
            t.process(b"out\r\n");
        }
        // No command mark exists: nothing ever emitted 133;D.
        assert!(
            t.command_marks().is_empty(),
            "an A/B-only shell records no completed-command marks — that is the \
             whole point of the union"
        );
        let prompts: Vec<u64> = t.all_blocks().map(|b| b.prompt_start_row).collect();
        assert_eq!(prompts.len(), 2, "two prompts were marked: {prompts:?}");

        t.scroll_to_bottom();
        let top = t.grid().top_visible_absolute_row();
        let older: Vec<u64> = prompts.iter().copied().filter(|&r| r < top).collect();
        assert!(
            !older.is_empty(),
            "the fixture must leave a prompt above the viewport: top={top} \
             prompts={prompts:?}"
        );
        assert_eq!(
            jump_prompt_target(&t, true),
            older.iter().copied().max(),
            "prev-prompt must reach the nearest prompt above the viewport"
        );

        // And from the top, next-prompt walks forward again.
        t.scroll_to_top();
        let top = t.grid().top_visible_absolute_row();
        assert_eq!(
            jump_prompt_target(&t, false),
            prompts.iter().copied().filter(|&r| r > top).min(),
            "next-prompt must reach the nearest prompt below the viewport"
        );
    }

    /// The union must not change a fully-marked shell: blocks and command marks
    /// carry the SAME `prompt_start_row`, so bash/zsh/fish/pwsh/wsl land exactly
    /// where they did before.
    #[test]
    fn jump_prompt_target_is_unchanged_for_a_fully_marked_shell() {
        let mut t = Terminal::new(4, 20);
        for n in 0..3 {
            t.process(b"\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\cmd\r\n\x1b]133;C\x1b\\");
            for _ in 0..5 {
                t.process(b"out\r\n");
            }
            let _ = n;
            t.process(b"\x1b]133;D;0\x1b\\");
        }
        let marks: Vec<u64> = t
            .command_marks()
            .iter()
            .map(|m| m.prompt_start_row)
            .collect();
        assert_eq!(marks.len(), 3, "three completed commands: {marks:?}");
        t.scroll_to_bottom();
        let top = t.grid().top_visible_absolute_row();
        // The mark-only answer and the union answer must agree.
        assert_eq!(
            jump_prompt_target(&t, true),
            marks.iter().copied().filter(|&r| r < top).max()
        );
        assert_eq!(
            jump_prompt_target(&t, false),
            marks.iter().copied().filter(|&r| r > top).min()
        );
    }

    /// `report_coords` is the SOLE coordinate selector: pixel for 1016, cell for
    /// every other encoding, with the sub-cell offset clamped inside the cell so a
    /// stray over-range offset can't bleed into the next cell's pixel range.
    #[test]
    fn report_coords_selects_pixel_only_for_1016() {
        let mut t = Terminal::new(24, 80);
        t.set_cell_pixel_size(10, 20);
        // Cell encodings ignore the offset entirely.
        t.process(b"\x1b[?1000h");
        t.process(b"\x1b[?1006h"); // SGR (cell)
        assert_eq!(report_coords(&t, 3, 2, PixelOffset { x: 9, y: 19 }), (3, 2));
        // 1016: pixel = cell origin + (clamped) offset.
        t.process(b"\x1b[?1016h");
        assert_eq!(
            report_coords(&t, 3, 2, PixelOffset { x: 4, y: 7 }),
            (34, 47)
        );
        // An offset at/over the cell size is clamped to the last in-cell pixel
        // (cw-1 / ch-1), so it never crosses into the next cell's range.
        assert_eq!(
            report_coords(&t, 3, 2, PixelOffset { x: 99, y: 99 }),
            (3 * 10 + 9, 2 * 20 + 19)
        );
    }

    /// A drop of a plain path (only shell-inert bytes) is returned BYTE-FOR-BYTE
    /// unchanged — no spurious escaping, exactly what iTerm2 inserts for a tame
    /// path. Dots, slashes, dashes, underscores, colons, commas, percent, digits
    /// and non-ASCII letters all pass through.
    #[test]
    fn shell_escape_path_leaves_plain_paths_unchanged() {
        assert_eq!(
            shell_escape_path_posix("/home/user/Downloads/report-2026_v2.final.pdf"),
            "/home/user/Downloads/report-2026_v2.final.pdf"
        );
        assert_eq!(shell_escape_path_posix("/tmp/a:b,c%20d"), "/tmp/a:b,c%20d");
        assert_eq!(
            shell_escape_path_posix("/Users//me/Café/Ünïcødé.txt"),
            "/Users//me/Café/Ünïcødé.txt"
        );
        assert_eq!(shell_escape_path_posix(""), "");
    }

    /// The everyday case: spaces and parentheses (e.g. `My File (1).png`) are
    /// backslash-escaped so the path stays a single argument, matching iTerm2.
    #[test]
    fn shell_escape_path_escapes_spaces_and_parens() {
        assert_eq!(
            shell_escape_path_posix("/Users//me/My File (1).png"),
            "/Users//me/My\\ File\\ \\(1\\).png"
        );
    }

    /// A literal backslash in a name becomes exactly TWO backslashes — escaped
    /// once, never doubled-again, because the pass reads input chars only.
    #[test]
    fn shell_escape_path_escapes_backslash_once() {
        assert_eq!(shell_escape_path_posix("a\\b"), "a\\\\b"); // `a\b` -> `a\\b`
        assert_eq!(shell_escape_path_posix("\\"), "\\\\"); // `\` -> `\\`
    }

    /// EVERY character in iTerm2's drop escape set (shellEscapableCharacters plus
    /// CR/LF) is backslash-prefixed, and nothing else is. This pins the exact set
    /// so a future edit that drops or adds a metacharacter fails loudly.
    #[test]
    fn shell_escape_path_matches_iterm_set_exactly() {
        let set = "\\ ()\"&'!$<>;|*?[]#`\t{}^+=@~\r\n";
        let escaped = shell_escape_path_posix(set);
        // Output is exactly each set char prefixed by a backslash, in order.
        let expected: String = set.chars().flat_map(|c| ['\\', c]).collect();
        assert_eq!(escaped, expected);
        // A string of only inert chars gains no backslashes at all.
        let inert = "/.-_,:%0123456789abzABZ";
        assert_eq!(shell_escape_path_posix(inert), inert);
    }

    /// ADVERSARIAL: a filename crafted to break out of the argument and run code
    /// is fully neutralised — every shell-significant byte in the OUTPUT is
    /// preceded by its escaping backslash, so when inserted at the prompt it is an
    /// inert single argument, never a command substitution / pipe / redirection.
    #[test]
    fn shell_escape_path_neutralises_injection() {
        let attack = "/tmp/$(touch pwned);rm -rf ~ `id` | tee >out & evil!.txt";
        let escaped = shell_escape_path_posix(attack);
        let bytes: Vec<char> = escaped.chars().collect();
        for (i, &c) in bytes.iter().enumerate() {
            if matches!(
                c,
                '(' | ')' | '$' | ';' | '|' | '&' | '`' | '>' | '<' | '!' | ' ' | '~'
            ) {
                assert!(
                    i > 0 && bytes[i - 1] == '\\',
                    "unescaped {c:?} at {i} in {escaped:?}"
                );
            }
        }
    }

    /// END-TO-END through the paste seam: the drop site appends one space and
    /// routes the escaped path through `format_paste`. With bracketed paste OFF
    /// the bytes are the escaped path + trailing space verbatim (ESC/C1 are still
    /// stripped); with DEC 2004 ON they are wrapped in the `ESC[200~ … ESC[201~`
    /// guards — exactly like Cmd-V, and exactly what iTerm sends a 2004-mode app.
    ///
    /// STILL `#[cfg(unix)]`: `shell_escape_path` DISPATCHES on the platform, and the
    /// expected bytes here are the POSIX backslash form (`/p/My\ File`). Windows
    /// drops quote instead (`shell_escape_path_windows`), which
    /// `shell_escape_path_windows_*` already pins on every target; only the
    /// end-to-end paste bytes for the POSIX form live here.
    #[cfg(unix)]
    #[test]
    fn dropped_path_pastes_escaped_with_trailing_space() {
        let drop_text = |p: &str| {
            let mut t = shell_escape_path(p);
            t.push(' ');
            t
        };
        // Bracketed paste OFF: literal escaped path + trailing space.
        let term = term_with(&[]);
        assert_eq!(
            egress_bytes(&term, &InputEvent::Paste(drop_text("/p/My File"))),
            b"/p/My\\ File ".to_vec()
        );
        // Bracketed paste ON (DEC 2004): same body inside the bracket guards.
        let term = term_with(&[b"\x1b[?2004h"]);
        assert_eq!(
            egress_bytes(&term, &InputEvent::Paste(drop_text("/p/My File"))),
            b"\x1b[200~/p/My\\ File \x1b[201~".to_vec()
        );
    }

    /// winit delivers one `DroppedFile` per file with no batch boundary; pasting
    /// each as `escaped-path + space` reproduces iTerm2's space-joined output with
    /// a single trailing space and NO leading space — for any file count.
    #[test]
    fn multi_file_drop_concatenates_like_iterm() {
        let drop_text = |p: &str| {
            let mut t = shell_escape_path_posix(p);
            t.push(' ');
            t
        };
        let combined = drop_text("/a/one.txt") + &drop_text("/b/two three.txt");
        assert_eq!(combined, "/a/one.txt /b/two\\ three.txt ");
    }

    /// Windows drop quoting: a tame path — separators, drive colon, dots,
    /// dashes, underscores, non-ASCII letters — passes through verbatim, with
    /// backslashes NEVER doubled (`\` is the path separator, not an escape).
    #[test]
    fn shell_escape_path_windows_leaves_plain_paths_unchanged() {
        assert_eq!(
            shell_escape_path_windows("C:\\Users\\me\\report-2026_v2.final.pdf"),
            "C:\\Users\\me\\report-2026_v2.final.pdf"
        );
        assert_eq!(
            shell_escape_path_windows("C:\\Users\\me\\Café\\Ünïcødé.txt"),
            "C:\\Users\\me\\Café\\Ünïcødé.txt"
        );
        assert_eq!(shell_escape_path_windows(""), "");
    }

    /// THE flagship Windows case: a path under `Program Files` (embedded space)
    /// is double-quoted — one argument in PowerShell AND cmd — with every
    /// backslash intact, not POSIX backslash-escaped into
    /// `C:\\Program\ Files\\a.txt`.
    #[test]
    fn shell_escape_path_windows_quotes_spaces_keeping_separators() {
        assert_eq!(
            shell_escape_path_windows("C:\\Program Files\\a.txt"),
            "\"C:\\Program Files\\a.txt\""
        );
        assert_eq!(
            shell_escape_path_windows("C:\\Users\\me\\My File (1).png"),
            "\"C:\\Users\\me\\My File (1).png\""
        );
    }

    /// PowerShell/cmd metacharacters beyond space — comma (a PowerShell array
    /// separator!), `& | ; @ $ ' ~` — trigger the quotes; an embedded `"`
    /// (impossible in NTFS names, defensive for UNC/synthetic input) is doubled,
    /// the shared escaped-quote form of both shells.
    #[test]
    fn shell_escape_path_windows_quotes_metachars_and_doubles_quotes() {
        assert_eq!(shell_escape_path_windows("C:\\a,b.txt"), "\"C:\\a,b.txt\"");
        assert_eq!(
            shell_escape_path_windows("C:\\a&b;c|d.txt"),
            "\"C:\\a&b;c|d.txt\""
        );
        assert_eq!(shell_escape_path_windows("a\"b"), "\"a\"\"b\"");
    }

    /// The `shell_escape_path` entry point the drop site calls dispatches to the
    /// escaper for the HOST's shell family: Windows quoting on Windows, the
    /// POSIX backslash escaper everywhere else.
    #[test]
    fn shell_escape_path_dispatches_per_platform() {
        let spaced = "C:\\Program Files\\a.txt";
        #[cfg(windows)]
        assert_eq!(shell_escape_path(spaced), shell_escape_path_windows(spaced));
        #[cfg(not(windows))]
        assert_eq!(shell_escape_path(spaced), shell_escape_path_posix(spaced));
    }

    /// The CF_HDROP paste insertion (S9) is the drop_file contract verbatim:
    /// per-path `shell_escape_path` + ONE trailing space, space-joined for a
    /// multi-file copy, ending in a space — never a newline, so nothing runs.
    #[test]
    fn paths_paste_insertion_matches_the_drop_contract() {
        let one = vec!["C:\\Users\\me\\a.txt".to_string()];
        assert_eq!(
            paths_paste_insertion(&one),
            format!("{} ", shell_escape_path("C:\\Users\\me\\a.txt"))
        );
        // Multi-file: iTerm's `p1 p2 p3 ` shape — each escaped, each with its
        // own trailing space (so the join IS the separator).
        let many = vec![
            "C:\\Program Files\\a.txt".to_string(),
            "C:\\b (1).png".to_string(),
        ];
        let expect = format!(
            "{} {} ",
            shell_escape_path("C:\\Program Files\\a.txt"),
            shell_escape_path("C:\\b (1).png")
        );
        assert_eq!(paths_paste_insertion(&many), expect);
        assert!(expect.ends_with(' ') && !expect.contains('\n'));
        // Degenerate inputs insert nothing rather than a stray quoted "".
        assert_eq!(paths_paste_insertion(&[]), "");
        assert_eq!(paths_paste_insertion(&[String::new()]), "");
    }

    /// THE I12 DECISION TABLE: Shift bypasses tracking AND alt-scroll — before
    /// either test, so a shifted wheel can never reach `encode_mouse_wheel`
    /// (whose SHIFT_MASK fold would leak a shifted report) — while the unshifted
    /// rows keep the pre-I12 precedence exactly. Pure, so it pins the contract with
    /// no PTY of any kind — the seam byte tests around it reach the same rows
    /// through a real `SinkWriter` on every platform ([`CaptureSink`]), but this one
    /// names the offending ROW when a precedence edit regresses.
    #[test]
    fn wheel_route_shift_bypasses_tracking_before_every_other_test() {
        use super::{WheelRoute, wheel_route};
        // Shift held: the viewport wins over EVERY terminal-mode combination.
        for tracking in [false, true] {
            for alt_scroll in [false, true] {
                assert_eq!(
                    wheel_route(true, false, tracking, alt_scroll),
                    WheelRoute::Viewport,
                    "shift must bypass (tracking={tracking}, alt_scroll={alt_scroll})"
                );
            }
        }
        // Unshifted: the pre-I12 ladder, unchanged — tracking first, then
        // DEC-1007 alt-scroll, then the local viewport.
        assert_eq!(wheel_route(false, false, true, false), WheelRoute::Report);
        assert_eq!(wheel_route(false, false, true, true), WheelRoute::Report);
        assert_eq!(wheel_route(false, false, false, true), WheelRoute::AltScroll);
        assert_eq!(wheel_route(false, false, false, false), WheelRoute::Viewport);
        // SELECTION CUSTODY Phase 2: Alt on the main screen reaches local
        // history over a tracking app — and over nothing else, because the
        // caller only sets `alt_local` off the alt screen, where `alt_scroll`
        // can never be true at the same time in practice; the table still
        // answers every combination.
        assert_eq!(wheel_route(false, true, true, false), WheelRoute::Viewport);
        assert_eq!(wheel_route(false, true, false, false), WheelRoute::Viewport);
        assert_eq!(wheel_route(false, true, true, true), WheelRoute::Viewport);
        // Shift and Alt agree on the destination; either alone suffices.
        assert_eq!(wheel_route(true, true, true, true), WheelRoute::Viewport);
    }
}
