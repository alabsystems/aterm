// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The engine-neutral [`InputEvent`] vocabulary and the [`Source`] audit tag for the
//! [`App::input`](crate::App::input) convergence seam (design Addendum A.2,
//! `docs/design/HIERARCHICAL_SESSIONS.md`).
//!
//! BEFORE the seam (2026-06-16), human input (the winit `on_key`/`on_mouse_*`/
//! `on_cursor_moved`/`on_resize`/`on_focus` handlers) and controller input (the
//! control verbs `key`/`ctrl`/`mouse`/`scroll`/`paste`/`resize`/...) flowed through TWO parallel code paths that drifted: only the
//! human path read real modifiers, ran the click-count FSM, emitted intermediate
//! motion reports, snapped-to-bottom + cleared-selection on a keystroke, carried
//! the Kitty base-layout key, reported focus, and reset the cursor blink. The
//! controller path hard-coded `mods=0`, had no click-count, jumped straight to a
//! selection result, dropped events when tracking was off, and never snapped/
//! cleared/reset-blink.
//!
//! `snap_to_bottom` is not seam-exclusive: the paste and echo arms that never reach the seam
//! snap for themselves (`app_input::input_paste`, `dispatch_action`'s `Action::Paste`, the ⌘-V
//! and IME-composition arms of `on_key`, `on_ime_commit`'s paste arm). A press that writes no
//! PTY bytes may not take the user's reading position. The predictive-echo gate reads
//! `Terminal::kitty_suppresses_predictive_echo()`, a display-only projection that never feeds an
//! encoder. The seam ends at the `SinkWriter` ([`EgressMode`]): whole frames in byte order, and
//! the UI thread never parks on a wedged foreground's full tty input buffer.
//!
//! `Source` is audit-only: the seam never branches behaviour on it (the indistinguishability
//! invariant). The byte-producing core [`seam_egress_receipt`] takes no `Source`, and the
//! gesture-state arms of `App::input` read only data carried on the event (never `self.mods`).
//! The Tier-1 tests prove convergence: the human builders and the control-verb parsers produce
//! equal events for the same intent, and those events produce byte-identical sink output.

use std::sync::Mutex;

use aterm_core::selection::SelectionSide;
use aterm_core::terminal::{ModeMirror, Terminal};
use aterm_session::sink::{AcceptedOrder, BulkMeter, SinkWriter, WriteReceipt, WriteReceiptError};
use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey, encode_key_with_event};
use aterm_types::mouse::MouseButton;

use crate::term_lock;

/// One logical input event, engine-neutral. Built identically by a winit handler
/// (`Source::Human`) and by a control verb (`Source::Controller`); the seam turns it into PTY
/// bytes and viewport side effects the same way for both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InputEvent {
    /// A key chord. `base_layout` is the US-QWERTY equivalent of the physical key, fed to
    /// `encode_key_with_text` so Kitty `REPORT_ALTERNATE_KEYS` carries the third CSI-u field
    /// identically for both sources.
    Key {
        key: Key,
        mods: Modifiers,
        base_layout: Option<char>,
        /// The character the active layout typed for a text press, when it is not what the
        /// engine derives from `key` (the layout's unshifted key) on its own: German Shift+2's
        /// `"`, Caps Lock's `A` (`keymap::layout_text` builds it from winit's composed text).
        /// `encode_key_with_text` writes it for a text press and keeps `key` for kitty key codes
        /// and every Ctrl/Alt chord. `None` from a controller and whenever the US derivation
        /// agrees, so both sources build equal events for the same intent on a US layout.
        layout_text: Option<char>,
        /// Press / Repeat / Release. The human path builds `Press`, and `Release` for a key-up
        /// (encoded only under Kitty event-type reporting); the `key` verb's
        /// `type=press|repeat|release` lets a controller drive the Kitty event-type CSI-u
        /// sub-field (`:1`/`:2`/`:3`). Encoded once by `seam_egress`.
        event_type: aterm_types::keyboard::KeyEventType,
    },
    /// Literal text to type: the `on_key` bare-`ev.text` fallback and the IME
    /// `Ime::Commit` path (a commit holding a control character beside other
    /// text is a [`Self::Paste`] instead, `keymap::committed_text_is_paste`;
    /// one C0 or DEL character alone is a key and stays here). Each char is
    /// encoded as a `Character` key under the current keyboard mode.
    Text(String),
    /// Raw bytes written to the PTY verbatim, never re-encoded: an aterm `[key_sequences]` rule
    /// (a mapped chord sends exactly the bytes the user chose, whatever the keyboard mode), the
    /// `send`/`feed` verbs' bytes, bytes aterm types itself (a report, the Claude Code lights'
    /// Shift+Tab), or the ordered writer's copy of bytes the seam encoded.
    KeySequence(Vec<u8>),
    /// A mouse button press/release at a grid cell. `mods` is the real modifier mask,
    /// `click_count` the authoritative 1..=3 multi-click depth, `side` the cell-half for
    /// selection boundaries, and `block` the selection-type intent for a single-click press.
    MouseButton {
        button: MouseButton,
        pressed: bool,
        row: u16,
        col: u16,
        mods: u8,
        click_count: u8,
        side: SelectionSide,
        /// Single-click press starts a `Block` (rectangular) selection rather than `Simple`.
        /// Human: `self.mods.alt_key()` snapshotted at build time; controller: the `block=…`
        /// token (default `false`). Read only as data on the event.
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
        /// `seam_egress` ignores it entirely, so the human/controller byte-equality
        /// invariant is untouched.
        suppress_copy_on_select: bool,
        /// Sub-cell pixel offset inside the (`row`,`col`) cell, for DEC 1016 SGR-pixel mode
        /// (the winit cursor x/y minus the cell origin). The seam adds the cell origin and the
        /// engine's cell pixel size only when 1016 is active; cell encodings ignore it. Human:
        /// the real offset; controller: `(0, 0)`.
        px_off: PixelOffset,
    },
    /// Pointer motion. `buttons == 3` is a no-button hover (motion report code 3), anything
    /// else a held-button drag. `side` is the cell-half.
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
    /// line when tracking is on, else the viewport scrolls `lines`, a detent's
    /// worth of them per detent: see `detent`). `lines` is clamped to `>= 1` in the
    /// seam so a non-positive count can never produce a silent human/controller
    /// asymmetry.
    ///
    /// `dir` is four-way, so a tilt wheel or horizontal swipe reaches an app as xterm's buttons
    /// 66/67. The horizontal half is report-only: aterm's viewport has no horizontal axis, so
    /// with tracking off a horizontal wheel moves nothing (the `Wheel` arm of `seam_egress`).
    Wheel {
        dir: aterm_types::mouse::WheelDir,
        lines: i32,
        /// Whether `lines` counts wheel DETENTS (winit's `LineDelta`: a notch, an X11
        /// scroll-valuator step, the `mouse` verb) rather than rows a precise device
        /// already measured (a `PixelDelta` banked by the cell height). Only a detent
        /// is worth the platform's lines-per-detent ([`wheel_platform_lines`]) on the
        /// two surfaces aterm scrolls itself; a precise row is one row. It never
        /// changes the tracking reports, which stay one per `lines`.
        detent: bool,
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
    /// Paste text as if typed, framed per [`PasteFraming`] (bracketed when the
    /// app enabled DECSET 2004, unless the gesture already captured that answer).
    Paste(String, PasteFraming),
    /// A geometry change, re-clamped against `MAX_GRID_*` in the seam.
    ///
    /// `echo_to_window` is a transport flag, not a `Source` branch: the control `resize` verb
    /// sets it so the seam also asks the window to match the new grid's pixel size (the verb has
    /// no window event of its own); the winit `Resized` handler clears it, since the window
    /// already has that size and re-requesting it would fight an interactive edge drag.
    Resize {
        rows: u16,
        cols: u16,
        echo_to_window: bool,
    },
    /// A window-pixel geometry change: ask the OS window for this inner size and let the grid
    /// follow from the `Resized` event the platform delivers, as an edge drag does.
    ///
    /// Not `Resize` in other units: `Resize` applies the grid first and then echoes the pixel
    /// size, so its window event finds the columns already agreeing and takes
    /// `on_resize_throttled`'s row-only branch, never the width throttle, its coalescing or its
    /// trailing settle. Here nothing is pre-applied, so the grid comes from the window event by
    /// the code a drag runs, and a few in a row look like a drag to the event loop. The one
    /// difference from a hand drag: AppKit is not in its event-tracking run-loop mode.
    ResizeWindowPx { width: u32, height: u32 },
    /// Focus gained/lost, for DEC 1004 focus reporting. `true` = focus-in.
    Focus(bool),
}

impl InputEvent {
    /// This event with a keypad key folded onto its main-block twin (`Key::main_block_twin`):
    /// `Numpad5` -> `Character('5')`, `NumpadEnter` -> `Enter`, `NumpadEnd` -> `End`. `None`
    /// when there is nothing to fold: a main-block key, a non-key event, or `NumpadBegin`.
    ///
    /// For consumers that read the key for its meaning, not its bytes: the native pages
    /// (`App::native_input_event`) and the seam's press classifier (`classify_press` /
    /// `is_plain_enter`). The PTY encoder never sees the folded event: the keypad identity is
    /// what DECKPAM's SS3 forms and kitty disambiguate report.
    ///
    /// A keypad glyph's twin carries itself as its `layout_text`: Shift picks a keypad level and
    /// never composes, so Shift+KP_5 types '5', not the main row's '%'.
    pub(crate) fn keypad_folded(&self) -> Option<InputEvent> {
        let InputEvent::Key {
            key,
            mods,
            base_layout,
            layout_text,
            event_type,
        } = self
        else {
            return None;
        };
        let key = key.main_block_twin()?;
        let layout_text = match key {
            Key::Character(glyph) => Some(glyph),
            _ => *layout_text,
        };
        Some(InputEvent::Key {
            key,
            mods: *mods,
            base_layout: *base_layout,
            layout_text,
            event_type: *event_type,
        })
    }
}

/// The character a text press of base key `c` types: the layout's own when the window carried
/// it (`InputEvent::Key::layout_text`), else the engine's US derivation (Shift's table, Caps
/// Lock's capital), the glyph the encoder writes for it.
#[must_use]
pub(crate) fn typed_glyph(c: char, mods: Modifiers, layout_text: Option<char>) -> char {
    layout_text.unwrap_or_else(|| aterm_types::keyboard::typed_character(c, mods))
}

/// Sub-cell pixel offset of the pointer inside its grid cell, carried on every mouse event so the
/// seam can produce a pixel coordinate for DEC 1016 (SGR-pixel) mode without reading winit state.
///
/// `x`/`y` are device pixels from the cell's top-left corner; with the cell origin and the
/// engine's cell pixel size the seam reconstructs the cursor pixel. Only the 1016 encoder reads
/// them. A controller sends [`PixelOffset::CELL_ORIGIN`], so its 1016 press still lands on the
/// right cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PixelOffset {
    /// Horizontal device pixels from the cell's left edge (`0..cell_w`).
    pub x: u16,
    /// Vertical device pixels from the cell's top edge (`0..cell_h`).
    pub y: u16,
}

impl PixelOffset {
    /// The cell's top-left corner: the offset a controller (no real pointer) uses.
    pub(crate) const CELL_ORIGIN: Self = Self { x: 0, y: 0 };
}

/// When the bracketed-paste (DEC 2004) question is answered for one [`InputEvent::Paste`],
/// carried on the event because a queue separates the gesture from the write.
///
/// `App::input_paste` enqueues a paste on the per-session ordered writer, and `seam_egress`
/// produces its bytes later on that writer's thread; the program can flip DEC 2004 in between.
/// The multi-line paste confirmation judges by the mode at the gesture, so framing by the mode
/// at the write could let a body judged inert because bracketed paste was on arrive unbracketed
/// at a bare prompt and submit every line unasked.
///
/// This is DATA on the event, never a [`Source`] branch: `seam_egress` reads the
/// same field for a human paste and a controller one, so identical events still
/// produce identical bytes and the human/controller byte-equality invariant is
/// untouched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PasteFraming {
    /// Read DEC 2004 when the bytes are produced: the `paste`/`paste-bin` verbs (no gesture, no
    /// confirmation; their contract is the mode at write time) and the file drop, which never
    /// consults the mode.
    AtDrain,
    /// Frame by `bracketed` whatever the mode says at drain time: the answer read when a
    /// person's paste gesture was judged, the read the pastejacking guard decided by.
    Gesture { bracketed: bool },
}

impl PasteFraming {
    /// The framing for a paste being written now, given the terminal's live DEC 2004 state: the
    /// one place the two answers are reconciled.
    #[must_use]
    pub(crate) fn bracketed(self, live: bool) -> bool {
        match self {
            Self::AtDrain => live,
            Self::Gesture { bracketed } => bracketed,
        }
    }
}

/// Tracking-agnostic scrollback navigation for [`InputEvent::ScrollView`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScrollIntent {
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
    /// Jump the viewport to the previous (older) recorded OSC 133 prompt row above the current
    /// top visible row (see [`jump_prompt_target`]); a no-op when there is none.
    PrevPrompt,
    /// Jump the viewport to the next (newer) recorded prompt row below the current top visible
    /// row; a no-op when there is none.
    NextPrompt,
}

/// The target absolute row for a prompt-to-prompt jump: the nearest recorded prompt row strictly
/// above (`prev`) or below the current top visible row, or `None` when there is none. Shared by
/// the seam's `ScrollView` handler and the `scroll` verb so the two cannot drift.
///
/// Prompt rows come from both indexes. `command_marks()` is filled only by OSC 133;D
/// (`shell_command_finished`), so alone it indexes finished commands, not prompts: cmd.exe has
/// no hook between Enter and the command running and emits A/B with no C/D. `all_blocks()`
/// carries `prompt_start_row` from the instant A arrives, the in-progress block included. For a
/// fully-marked shell the two agree row for row.
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

/// Who produced an [`InputEvent`]. Attribution only: the seam never branches on it for a byte
/// (the Tier-1 indistinguishability invariant). Its one reader is who-typed bookkeeping after
/// the egress: a `Human` press, text, paste, click, drag, wheel or view scroll stamps the
/// session's `human_ms=` (`app_input::note_person`), which the supervisor reads to keep its
/// hands off a person. Only the in-thread winit handlers build `Human`; the session owner's
/// connection is a `Controller`.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Source {
    /// An in-thread winit handler (real keyboard/mouse/focus on this window).
    Human,
    /// A control-socket verb, or input aterm sends on its own after a person's gesture (the
    /// Claude Code lights' follow-up keys, `crate::claude_lights`). No person made those at that
    /// instant, and stamping them would hold the supervisor off the session for its person
    /// grace after every automatic step.
    Controller,
}

/// The reply a reply-bearing verb gets back from the seam. Fire-and-forget
/// callers ignore it. `Copy` so the drain loop can keep the last outcome cheaply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InputOutcome {
    /// Applied (bytes written and/or viewport moved).
    Ok,
    /// A `Resize` fell outside `1..=MAX_GRID_*` and was not applied.
    RangeRejected,
    /// The encoded bytes were not (fully) written to the PTY: a short write (peer closed
    /// mid-frame), a full ordered-input queue, or a hard error. `OK` means delivered.
    WriteFailed,
    /// The session's input queue refused the event and NOTHING of it was written
    /// (a refused receipt with no accepted prefix): the program has not read its
    /// queued input. One answer on every route, the front tab's seam and a
    /// background control lane alike: `ERR busy input-queue-full`, retryable as is.
    QueueFull,
}

impl InputOutcome {
    /// Whether the event's bytes did not (all) reach the PTY: a failed write or a
    /// refusal at the input cap.
    pub(crate) const fn failed(self) -> bool {
        matches!(self, Self::WriteFailed | Self::QueueFull)
    }
}

/// Whether [`seam_egress_receipt`] delivered the event's encoded bytes to the PTY. An event that
/// encodes to no bytes (a legacy-mode key release, an unencodable modifier) is [`Full`]: nothing
/// was lost. Only a short or failed write is [`Failed`].
///
/// [`Full`]: Delivery::Full
/// [`Failed`]: Delivery::Failed
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// Every intended byte reached the PTY (or there were none to write).
    Full,
    /// A short or failed PTY write — the bytes did not (fully) land.
    Failed,
    /// The immediate actuator path accepted zero bytes and queued nothing.
    BusyZero,
    /// The immediate actuator path handed this prefix to the kernel and queued
    /// no tail.  The mutation is in-doubt and must not be retried.
    PartialInDoubt { accepted: usize },
}

impl Delivery {
    #[must_use]
    pub(crate) const fn is_full(self) -> bool {
        matches!(self, Self::Full)
    }
}

/// Classify a sink write result against the intended frame length. A partial write (`Ok(n)`,
/// `n < intended`: the peer closed mid-frame) fails like a hard error. A frame the sink spilled
/// (tty input buffer full) reports `Ok(len)` and counts as [`Delivery::Full`] though the kernel
/// has not consumed it; if the session dies first, those bytes die with it.
fn delivered(res: std::io::Result<usize>, intended: usize) -> Delivery {
    match res {
        Ok(n) if n == intended => Delivery::Full,
        _ => Delivery::Failed,
    }
}

/// One sink write's verdict: the delivery, the accepted order, whether the frame went whole to
/// the kernel directly, and whether the UI thread's egress refused it rather than park
/// ([`WriteReceiptError::is_refused`]).
fn delivered_receipt(
    result: Result<WriteReceipt, WriteReceiptError>,
    intended: usize,
) -> (Delivery, Option<AcceptedOrder>, bool, bool) {
    match result {
        Ok(receipt) => (
            delivered(Ok(receipt.accepted()), intended),
            receipt.order(),
            receipt.is_direct(),
            false,
        ),
        Err(error) => (Delivery::Failed, error.order(), false, error.is_refused()),
    }
}

/// What [`seam_egress_receipt`] did with a mouse/wheel event, so `App::input` knows whether the
/// tracking-off local fallback must still run. The byte decision is all `seam_egress`'s; the
/// viewport, gesture and window side effects stay in `App::input`, which has the renderer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Egress {
    /// The event produced a tracking report (or had no local fallback): the seam is done with
    /// it. Carries whether the bytes reached the PTY, so a reply-bearing caller is told the
    /// truth.
    Reported(Delivery),
    /// Mouse tracking is off: `App::input` runs the local fallback (a selection gesture for a
    /// button/move, a viewport scroll of `wheel_lines` for a wheel). A wheel's fallback carries
    /// the engine facts the scroll needs ([`WheelViewport`]), read under the seam's one lock;
    /// `None` from the button/move arms.
    TrackingOff {
        wheel_lines: i32,
        wheel_up: bool,
        viewport: Option<WheelViewport>,
    },
}

/// The engine facts a tracking-off wheel's local viewport scroll consumes, snapshotted under the
/// seam's single `term_lock` when the route was decided, so `input_wheel` takes no further
/// acquisition (each a fresh queue position behind the PTY reader's `process()` slice).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WheelViewport {
    /// `Grid::display_offset()` — the viewport's current row into history.
    pub display_offset: usize,
    /// `Grid::scrollback_lines()` — the clamp bound for the glide's target.
    pub scrollback_lines: usize,
    /// `Terminal::rows()` — the page for Windows' "One screen at a time" detent.
    pub rows: u16,
}

/// One egress verdict plus the order of its last conclusively accepted non-empty
/// PTY frame. [`Delivery::Full`] deliberately also describes faithful zero-byte
/// input (for example a legacy key release), and a multi-frame event's final
/// verdict can erase an earlier accepted frame, so neither fact can be recovered
/// from [`Egress`] alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EgressReceipt {
    pub(crate) egress: Egress,
    accepted_order: Option<AcceptedOrder>,
    /// The accepted frame reached the kernel whole on the sink's direct lane
    /// (`WriteReceipt::is_direct`), so the echo tracker can settle its spill debt without
    /// probing the spill mutex. Conservative `false` everywhere else.
    direct: bool,
    /// Refused because the session's input queue is full: the program stopped reading and the
    /// UI thread's egress would have parked (`WriteReceiptError::is_refused`), or the ordered
    /// writer did not admit it (`paste_order`). Always with `Delivery::Failed`; the App says so
    /// once per session (`App::refuse_input`).
    refused: bool,
    /// Handed to the per-session ordered writer ([`Self::deferred_full`]), which writes its
    /// bytes later. A report goes there only with bytes to write.
    queued: bool,
}

impl EgressReceipt {
    /// Adapt a delivery reported by a guarded seam that performs the same sink
    /// operation but must keep its terminal lock across an authorization check.
    #[must_use]
    pub(crate) const fn from_reported_delivery(
        delivery: Delivery,
        accepted_order: Option<AcceptedOrder>,
    ) -> Self {
        Self {
            egress: Egress::Reported(delivery),
            accepted_order,
            direct: false,
            refused: false,
            queued: false,
        }
    }

    /// An event the seam decided and nothing wrote: `egress` as the seam
    /// reported it (tracking off, or a report the mode swallows), no byte
    /// moved, nothing queued.
    #[must_use]
    pub(crate) const fn unwritten(egress: Egress) -> Self {
        Self {
            egress,
            accepted_order: None,
            direct: false,
            refused: false,
            queued: false,
        }
    }

    /// FIFO admission is not PTY acceptance: `Full` (the ordered serializer owns the event)
    /// with no byte receipt until that serializer does the real write.
    #[must_use]
    pub(crate) const fn deferred_full() -> Self {
        Self {
            egress: Egress::Reported(Delivery::Full),
            accepted_order: None,
            direct: false,
            refused: false,
            queued: true,
        }
    }

    /// The ordered writer refused the event: its session's input queue has no room, or its
    /// writer is gone (`paste_order`'s admission). No byte moved and none ever will, and it is
    /// never written around the FIFO.
    #[must_use]
    pub(crate) const fn queue_full() -> Self {
        Self {
            egress: Egress::Reported(Delivery::Failed),
            accepted_order: None,
            direct: false,
            refused: true,
            queued: false,
        }
    }

    /// Whether the event was refused because its session's input queue is
    /// full (see the field): the program is not reading, and the person should
    /// be told rather than lose the key silently.
    #[must_use]
    pub(crate) const fn input_queue_full(self) -> bool {
        self.refused
    }

    /// Whether the event was handed to the per-session ordered writer (see
    /// the field) rather than written, refused or dropped here.
    #[must_use]
    pub(crate) const fn is_queued(self) -> bool {
        self.queued
    }

    /// Whether at least one byte was conclusively accepted by the sink.
    ///
    /// Ordinary interactive/backpressured writes report only all-or-failed.
    /// The guarded actuator additionally preserves an explicit accepted prefix;
    /// that prefix may be echoed even though the whole frame was not delivered.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn accepted_nonempty(self) -> bool {
        self.accepted_order.is_some()
    }

    /// Whether the accepted frame went whole to the kernel on the direct lane.
    #[must_use]
    pub(crate) const fn is_direct(self) -> bool {
        self.direct
    }

    /// Accepted sink order, or `None` when this event moved no bytes.
    #[must_use]
    pub(crate) const fn accepted_order(self) -> Option<AcceptedOrder> {
        self.accepted_order
    }

    /// The whole non-empty event was accepted. A failed multi-frame event may
    /// still carry an earlier frame's order, which is insufficient to promise
    /// that a queued key can echo or request a fast present.
    #[must_use]
    pub(crate) const fn fully_accepted_nonempty(self) -> bool {
        matches!(self.egress, Egress::Reported(Delivery::Full)) && self.accepted_order.is_some()
    }
}

/// The `(x, y)` a mouse report carries under `term`'s current encoding: for DEC 1016
/// (SGR-pixel), the device-pixel coordinate rebuilt from the cell origin (`col * cell_w`,
/// `row * cell_h`, from the engine's `cell_pixel_size()`) plus the measured sub-cell `px_off`;
/// else the cell. 0-based either way (`encode_sgr` adds the `+1`), and saturating, so a huge
/// grid cannot wrap. Runs under the caller's `term_lock`.
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

/// Ceiling on how many times one wheel event may write its encoded bytes: the per-line mouse
/// reports under a tracking app, and the DEC 1007 arrow presses, which number `lines` times the
/// platform's lines-per-detent (Windows' "One screen at a time" multiplies by the viewport
/// height). Past 512 an event floods the PTY. Clamped where both sources converge, at the seam's
/// `lines` normalization (`control_input::MAX_WHEEL_LINES` is this constant): a trackpad's
/// horizontal deltas divide by the cell width, so one momentum event can bank many notches.
pub(crate) const MAX_WHEEL_BURST: i32 = 512;

/// How far a wheel gesture of `lines` travels, in lines: when `detent` is set
/// ([`InputEvent::Wheel::detent`]) `lines` counts DETENTS and the platform's
/// lines-per-detent is applied; otherwise they are rows a precise device already
/// measured, final on every platform. `page_rows` is the viewport height, which
/// sizes Windows' "One screen at a time" and the free desktops' detent.
///
/// A detent, per platform:
/// - WINDOWS: `SPI_GETWHEELSCROLLLINES` lines — default **3**, the Mouse-settings
///   slider, the distance every other window on the desktop travels. winit's Win32
///   backend reports exactly ±1.0 `LineDelta` per detent.
/// - LINUX and the BSDs (X11, Wayland): VTE's `ceil(rows / 10)` lines, at least one
///   — **3** at 24 rows, what GNOME Terminal and every VTE terminal scroll. Neither
///   protocol carries a lines-per-detent setting: X11's button 4/5 and a scroll
///   valuator's increment, and Wayland's `axis_discrete`, all reach winit as ±1.0
///   `LineDelta` a detent, and the toolkit picks the distance. An X11 touchpad's
///   smooth-scroll valuator arrives in the same detent units (fractional
///   `LineDelta`), and VTE scales it by the same factor; a Wayland touchpad arrives
///   as `PixelDelta`, which is not a detent and tracks the finger 1:1.
/// - macOS: the identity. AppKit folds the user's scroll speed into
///   `scrollingDeltaY` before winit sees it.
///
/// THE TWO CALLERS ARE THE TWO SURFACES THE USER SCROLLS WITHOUT AN APP'S HELP:
/// the local scrollback viewport (`App::input_wheel`), and the alternate-scroll
/// (DEC 1007) arrow synthesis in `seam_egress`, which is how `less`, `man` and
/// `git log` scroll. The mouse-TRACKING path deliberately does NOT call this: there
/// the app receives real wheel reports and applies its own notch->lines conversion
/// (vim's `mousescroll`, default 3), so scaling would compound to nine lines a
/// notch.
///
/// `platform` is the rule a detent scrolls by ([`WheelPlatform`]): production
/// passes [`WheelPlatform::Host`], a test any platform's arm.
pub(crate) fn wheel_platform_lines(
    platform: WheelPlatform,
    lines: i32,
    page_rows: u16,
    detent: bool,
) -> i32 {
    if detent {
        platform.detent_lines(lines, page_rows)
    } else {
        lines
    }
}

/// How far ONE wheel notch travels, per the user's Windows mouse settings
/// (`platform_win::wheel_notch_scroll` reads it). [`WheelNotch::Page`] is the
/// "One screen at a time" slider position, which has no line count — the
/// consumer supplies its own viewport height. Platform-neutral, so Windows'
/// arm of [`WheelPlatform`] runs on every host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(any(test, windows)),
    expect(
        dead_code,
        reason = "read from the Mouse setting on Windows and injected by tests; no other host \
                  builds one"
    )
)]
pub(crate) enum WheelNotch {
    /// `n` lines a detent; `0` is "wheel scrolling off", honoured as no motion.
    Lines(u32),
    /// A page a detent.
    Page,
}

/// THE PLATFORM'S WHEEL RULE (session log phase 6, 6A-4): how many lines one
/// wheel detent scrolls a surface that scrolls without an app's help
/// ([`wheel_platform_lines`]: the local viewport and alternate scroll's
/// arrows). INJECTED, as AGENTS.md has platform probes: the seam's core and
/// [`crate::human_input::reaches_program`] take it as a value, production
/// passes [`WheelPlatform::Host`], which reads this host's rule only where a
/// detent needs it, and a test passes any platform's arm, which reads nothing.
/// So Windows' "wheel scrolling off", the one arm under which a vertical
/// wheel on alternate scroll sends a pager nothing, runs on every host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "each host builds only its own arm (`host_wheel_platform`); the other \
                  platforms' arms are the ones a test injects"
    )
)]
pub(crate) enum WheelPlatform {
    /// This host's own rule, read where a detent needs it: Windows' live Mouse
    /// setting, the free desktops' VTE distance, macOS's final delta.
    Host,
    /// macOS: AppKit folds the user's scroll speed into the delta before winit
    /// sees it, so the gesture's line count is final.
    Mac,
    /// Linux and the BSDs: VTE's distance ([`vte_lines_per_detent`]).
    Vte,
    /// Windows, on the Mouse setting `notch` ([`wheel_scaled_lines`]).
    Windows(WheelNotch),
}

impl WheelPlatform {
    /// The lines `notch_lines` detents scroll a surface `page_rows` tall, by
    /// this rule ([`wheel_platform_lines`] for a detent).
    pub(crate) fn detent_lines(self, notch_lines: i32, page_rows: u16) -> i32 {
        let rule = if self == WheelPlatform::Host {
            host_wheel_platform()
        } else {
            self
        };
        match rule {
            WheelPlatform::Host | WheelPlatform::Mac => notch_lines,
            WheelPlatform::Vte => notch_lines.saturating_mul(vte_lines_per_detent(page_rows)),
            WheelPlatform::Windows(notch) => wheel_scaled_lines(notch_lines, notch, page_rows),
        }
    }

    /// Every arm a test injects: macOS, the free desktops, and Windows on the
    /// default three lines, "wheel scrolling off" and "One screen at a time".
    #[cfg(test)]
    pub(crate) const ARMS: [WheelPlatform; 5] = [
        WheelPlatform::Mac,
        WheelPlatform::Vte,
        WheelPlatform::Windows(WheelNotch::Lines(3)),
        WheelPlatform::Windows(WheelNotch::Lines(0)),
        WheelPlatform::Windows(WheelNotch::Page),
    ];
}

/// [`WheelPlatform::Host`] on Windows: the live Mouse setting.
#[cfg(windows)]
fn host_wheel_platform() -> WheelPlatform {
    WheelPlatform::Windows(crate::platform_win::wheel_notch_scroll())
}

/// [`WheelPlatform::Host`] on the free desktops: VTE's distance.
#[cfg(all(unix, not(target_os = "macos")))]
fn host_wheel_platform() -> WheelPlatform {
    WheelPlatform::Vte
}

/// [`WheelPlatform::Host`] on macOS: the delta is final.
#[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
fn host_wheel_platform() -> WheelPlatform {
    WheelPlatform::Mac
}

/// The lines one wheel detent scrolls in a VTE terminal (GNOME Terminal, Tilix,
/// Terminator, …) showing `page_rows` rows: `ceil(page_rows / 10)`, at least one.
/// VTE's `Terminal::widget_mouse_scroll` takes it from the vertical adjustment's
/// page increment, which is the row count; one detent is 3 lines at 24 rows, 5 at
/// 50, and 1 below 11.
fn vte_lines_per_detent(page_rows: u16) -> i32 {
    i32::from(page_rows.div_ceil(10)).max(1)
}

/// Where one wheel event goes: the seam's routing decision, factored out of `seam_egress` so the
/// precedence is a pure truth table (the byte production stays in the seam, under its one lock).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WheelRoute {
    /// An app is tracking the mouse: encode wheel reports for it.
    Report,
    /// Alt screen + DEC 1007, tracking off: synthesize arrow-key presses.
    AltScroll,
    /// aterm's own scrollback viewport (`Egress::TrackingOff`).
    Viewport,
}

/// Resolve a wheel's [`WheelRoute`] from the facts the seam reads under its term lock. The order
/// is the contract:
///
/// 1. `shift_held` wins over everything: Shift+wheel is the xterm convention for "scroll the
///    terminal, not the app" (the keyboard half is Shift+PgUp/PgDn, `scrollback_chord`), and
///    without it no wheel gesture moves the viewport over fzf or a mouse-enabled REPL. Checked
///    before tracking, so a shifted report never reaches `encode_mouse_wheel`, and before
///    DEC 1007, as xterm does. The platform lines-per-detent still applies
///    (`wheel_viewport_lines`).
/// 2. `alt_local`, Option/Alt held on the main screen: the wheel scrolls this terminal's
///    scrollback even under tracking, the wheel half of Option-click selection
///    (`press_starts_selection`). The caller scopes it to the main screen: the alt screen has
///    no scrollback, and Alt there keeps the DEC 1007 arrows.
/// 3. Mouse tracking: the app gets real reports.
/// 4. Alternate scroll (DEC 1007, alt screen): arrows for less/man/git-log.
/// 5. Otherwise: the local scrollback viewport.
///
/// The bypass lives here, not in `on_mouse_wheel`, so the controller `mouse` verb gets it too
/// (the seam is source-blind), and it is the same on every platform.
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

/// The four facts [`wheel_route`] decides on, read from one engine under the caller's lock; the
/// policy stays in `wheel_route`. `alt_local`'s main-screen scoping is here because it is a fact
/// about the engine (the alt screen has no scrollback).
pub(crate) fn wheel_route_for(t: &Terminal, mods: u8) -> WheelRoute {
    wheel_route(
        mods & aterm_types::mouse::SHIFT_MASK != 0,
        mods & aterm_types::mouse::ALT_MASK != 0 && !t.is_alternate_screen(),
        t.mouse_tracking_enabled(),
        t.is_alternate_screen() && t.modes().alternate_scroll,
    )
}

/// [`wheel_route_for`]'s lock-free twin: the same four facts from the session's published
/// [`ModeMirror`], which can trail a running `process()` batch by that one batch. Its one caller,
/// the parked-edge arm of `App::input_wheel`, only decides whether the seam is worth its lock: a
/// delta pushing a band already parked at a history end, on a `Viewport` route, moves nothing.
pub(crate) fn wheel_route_mirrored(mirror: &ModeMirror, mods: u8) -> WheelRoute {
    let alt_screen = mirror.alternate_screen();
    wheel_route(
        mods & aterm_types::mouse::SHIFT_MASK != 0,
        mods & aterm_types::mouse::ALT_MASK != 0 && !alt_screen,
        mirror.mouse_tracking_enabled(),
        alt_screen && mirror.alternate_scroll(),
    )
}

/// The arrow key alternate scroll (DEC 1007, [`WheelRoute::AltScroll`]) sends
/// for one wheel line turned `dir`: ArrowUp or ArrowDown, encoded through the
/// keyboard mode `keyboard` (DECCKM's SS3 arrows and the kitty forms stay
/// exact, never a hardcoded `ESC[A`), and empty where that mode encodes the
/// arrow to nothing. `None` for a horizontal wheel: alternate scroll is
/// VERTICAL ONLY (the reason is at the seam's arm). The seam writes these bytes
/// under its lock, and [`crate::human_input::reaches_program`] asks the same
/// rule, on the mode mirror's keyboard mode, whether they would be written.
pub(crate) fn alt_scroll_arrow(
    dir: aterm_types::mouse::WheelDir,
    keyboard: aterm_types::keyboard::KeyboardMode,
) -> Option<Vec<u8>> {
    let arrow = if dir.vertical_up()? {
        NamedKey::ArrowUp
    } else {
        NamedKey::ArrowDown
    };
    Some(encode_key_with_event(
        &Key::Named(arrow),
        Modifiers::empty(),
        keyboard,
        KeyEventType::Press,
    ))
}

/// How many [`alt_scroll_arrow`] presses one wheel of `lines` (already clamped
/// to `1..=MAX_WHEEL_BURST`) sends a pager showing `page_rows` rows: the
/// platform's lines-per-detent for a detent ([`wheel_platform_lines`]),
/// bounded by [`MAX_WHEEL_BURST`]. Zero only where the platform scrolls a
/// detent no lines at all (Windows' "wheel scrolling off", `Lines(0)`): every
/// per-detent factor that depends on the page is floored at one row (VTE's
/// `ceil(rows / 10)`, Windows' page), so the page height never decides whether
/// the count is zero, and the mode mirror, which holds no height, answers that
/// question with any ([`alt_scroll_sends`]).
pub(crate) fn alt_scroll_repeat(
    platform: WheelPlatform,
    lines: i32,
    page_rows: u16,
    detent: bool,
) -> i32 {
    wheel_platform_lines(platform, lines, page_rows, detent).clamp(0, MAX_WHEEL_BURST)
}

/// Whether one wheel of `lines` (as the event carries it; clamped here as the
/// seam clamps it) sends a pager any [`alt_scroll_arrow`] press at all:
/// [`alt_scroll_repeat`] is positive, for any page height (its docs).
pub(crate) fn alt_scroll_sends(platform: WheelPlatform, lines: i32, detent: bool) -> bool {
    alt_scroll_repeat(platform, lines.clamp(1, MAX_WHEEL_BURST), 1, detent) > 0
}

/// The pure arithmetic of Windows' arm of [`WheelPlatform`], split out so the
/// multiply can be tested without a live `SystemParametersInfoW` (whose answer
/// belongs to whoever's machine runs the suite, and so can never be asserted).
///
/// [`WheelNotch::Page`] is the slider's "One screen at a time" position: a detent is
/// then a PAGE, sized as the viewport so a wheel and PgUp/PgDn cannot disagree about
/// what a page is. `Lines(0)` — "wheel scrolling off" in Mouse settings — is
/// honoured as zero rather than clamped up to one: a user who turned the wheel off
/// means it.
fn wheel_scaled_lines(notch_lines: i32, notch: WheelNotch, page_rows: u16) -> i32 {
    let per_notch = match notch {
        WheelNotch::Lines(n) => i32::try_from(n).unwrap_or(i32::MAX),
        WheelNotch::Page => i32::from(page_rows).max(1),
    };
    notch_lines.saturating_mul(per_notch)
}

/// How [`seam_egress_receipt`] hands the encoded bytes to the [`SinkWriter`] — a TRANSPORT
/// knob keyed on the CALLING THREAD, never on who produced the event (cf.
/// `echo_to_window` and the `resize` arm, which key on WHERE the event came from,
/// not on [`Source`]). It NEVER changes WHICH bytes are produced, so the
/// human/controller byte-equality invariant holds for either variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EgressMode {
    /// The UI event-loop thread, and a control socket lane writing into a session that is not
    /// on screen: the non-parking egress (`SinkWriter::write_frame_interactive_with_receipt`),
    /// so a wedged foreground never parks the loop that serves every window, nor a lane other
    /// socket clients need. At the sink's spill cap the frame is refused ([`Delivery::Failed`],
    /// [`EgressReceipt::input_queue_full`]). On Windows keys and reports never take this mode
    /// inline: they go through the per-session ordered writer (`paste_order`).
    Interactive,
    /// An expendable egress thread (the per-session egress-order writer): the blocking,
    /// `SPILL_CAP`-enforcing egress, so a machine-rate producer into a wedged foreground feels
    /// backpressure here instead of growing the spill without bound. Never on the UI thread or
    /// a control socket lane.
    Backpressured,
    /// The UI thread where a write could block it (Windows: ConPTY has no non-blocking write),
    /// or behind a queued paste: produce exactly the bytes `Interactive` would and write
    /// nothing. [`seam_encode`] hands them back for the per-session ordered writer
    /// (`paste_order`'s report route), so no report waits on the kernel on the UI thread.
    Encode,
}

/// Deliver `bytes` to `sink` per `mode`. Transport only: the bytes are identical either way (see
/// [`EgressMode`]); only the parking discipline differs.
fn emit(sink: &SinkWriter, mode: EgressMode, bytes: &[u8], accepted: &mut Accepted) -> Delivery {
    debug_assert!(!bytes.is_empty());
    let (delivery, order, direct, refused) = match mode {
        EgressMode::Interactive => delivered_receipt(
            sink.write_frame_interactive_with_receipt(bytes),
            bytes.len(),
        ),
        EgressMode::Backpressured => {
            delivered_receipt(sink.write_frame_with_receipt(bytes), bytes.len())
        }
        EgressMode::Encode => {
            accepted.encoded.extend_from_slice(bytes);
            (Delivery::Full, None, false, false)
        }
    };
    if order.is_some() {
        accepted.order = order;
        accepted.direct = direct;
    }
    accepted.refused |= refused;
    delivery
}

/// [`emit`]'s `Backpressured` arm for a metered bulk frame (a large paste):
/// the same blocking, `SPILL_CAP`-enforcing write, reporting into `meter`.
fn emit_metered(
    sink: &SinkWriter,
    bytes: &[u8],
    accepted: &mut Accepted,
    meter: &BulkMeter,
) -> Delivery {
    let (delivery, order, direct, _) = delivered_receipt(
        sink.write_frame_metered_with_receipt(bytes, meter),
        bytes.len(),
    );
    if order.is_some() {
        accepted.order = order;
        accepted.direct = direct;
    }
    delivery
}

/// The seam's accepted-frame accumulator: the last accepted non-empty frame's order, whether it
/// went whole to the kernel directly, whether any frame was refused for a full input queue, and
/// (under [`EgressMode::Encode`] only) every frame's bytes, in order, that nothing wrote.
#[derive(Clone, Debug, Default)]
struct Accepted {
    order: Option<AcceptedOrder>,
    direct: bool,
    refused: bool,
    encoded: Vec<u8>,
}

/// [`seam_egress_receipt`] without the receipt: the spelling the byte-level
/// tests drive. Production reads the receipt (accepted order, refusal), so
/// every production entry is the receipt-bearing one.
#[cfg(test)]
pub(crate) fn seam_egress(
    term: &Mutex<Terminal>,
    modes: &ModeMirror,
    sink: &SinkWriter,
    ev: &InputEvent,
    mode: EgressMode,
) -> Egress {
    debug_assert_ne!(
        mode,
        EgressMode::Encode,
        "an encode-only seam is `seam_encode`"
    );
    let mut accepted = Accepted::default();
    seam_egress_inner(
        term,
        modes,
        sink,
        ev,
        mode,
        &mut accepted,
        None,
        WheelPlatform::Host,
    )
}

/// The source-blind byte-producing core of the seam (design A.2 / A.7): the sole caller of
/// `encode_key_with_text`, the `encode_mouse_*` family, `encode_committed_text`,
/// `format_paste_framed` and the focus-report egress. Each mode is read once per event: the
/// keyboard and focus modes lock-free from the published [`ModeMirror`], the mouse and paste
/// modes under one `term_lock`.
///
/// CRITICAL (the indistinguishability invariant): this function takes NO
/// [`Source`] — it is STRUCTURALLY impossible for it to branch on who produced
/// the event, so a Human and a Controller feeding the SAME `InputEvent` get
/// byte-identical output. [`EgressMode`] is a TRANSPORT knob (keyed on the calling thread, not the
/// source) and never touches the produced bytes, so the invariant is preserved.
///
/// Only the byte-producing arms are handled here; the viewport/gesture/clipboard/
/// blink/snap/resize side-effects (which need the renderer + window + gesture
/// state) stay in `App::input`, which calls this and then runs those.
///
/// It takes no [`Source`], so a Human and a Controller feeding the same `InputEvent` get
/// byte-identical output (`bytes_human_eq_controller`; the `Buggy` mutant is its negative
/// control). [`EgressMode`] is transport and never changes the bytes. Side effects that need the
/// renderer, window or gesture state stay in `App::input`. The receipt keeps the accepted-byte
/// evidence [`Egress`] erases for zero-byte and multi-frame events.
pub(crate) fn seam_egress_receipt(
    term: &Mutex<Terminal>,
    modes: &ModeMirror,
    sink: &SinkWriter,
    ev: &InputEvent,
    mode: EgressMode,
) -> EgressReceipt {
    seam_egress_receipt_metered(term, modes, sink, ev, mode, None)
}

/// [`seam_egress_receipt`] with a [`BulkMeter`] for a large paste: its one frame goes through
/// [`SinkWriter::write_frame_metered_with_receipt`], which reports progress and honours the
/// meter's stop. Transport only, like [`EgressMode`]: only a `Backpressured` paste uses it.
pub(crate) fn seam_egress_receipt_metered(
    term: &Mutex<Terminal>,
    modes: &ModeMirror,
    sink: &SinkWriter,
    ev: &InputEvent,
    mode: EgressMode,
    meter: Option<&BulkMeter>,
) -> EgressReceipt {
    debug_assert_ne!(
        mode,
        EgressMode::Encode,
        "an encode-only seam is `seam_encode`"
    );
    let mut accepted = Accepted::default();
    let egress = seam_egress_inner(
        term,
        modes,
        sink,
        ev,
        mode,
        &mut accepted,
        meter,
        WheelPlatform::Host,
    );
    EgressReceipt {
        egress,
        accepted_order: accepted.order,
        direct: accepted.direct,
        refused: accepted.refused,
        queued: false,
    }
}

/// [`seam_egress_receipt`] under [`EgressMode::Encode`]: the event's [`Egress`], as an inline
/// write would have reported it, and the bytes the seam produced, which nothing has written.
/// Empty when the event writes nothing (tracking off, a swallowed wheel, focus reporting off).
/// `sink` is not touched.
pub(crate) fn seam_encode(
    term: &Mutex<Terminal>,
    modes: &ModeMirror,
    sink: &SinkWriter,
    ev: &InputEvent,
) -> (Egress, Vec<u8>) {
    seam_encode_on(WheelPlatform::Host, term, modes, sink, ev)
}

/// [`seam_encode`] on the wheel rule `platform` ([`WheelPlatform`]): the one
/// seam, its detents scrolled by an injected platform's arm, so a test drives
/// every platform's wheel through it on any host.
pub(crate) fn seam_encode_on(
    platform: WheelPlatform,
    term: &Mutex<Terminal>,
    modes: &ModeMirror,
    sink: &SinkWriter,
    ev: &InputEvent,
) -> (Egress, Vec<u8>) {
    let mut accepted = Accepted::default();
    let egress = seam_egress_inner(
        term,
        modes,
        sink,
        ev,
        EgressMode::Encode,
        &mut accepted,
        None,
        platform,
    );
    (egress, accepted.encoded)
}

#[allow(
    clippy::too_many_arguments,
    reason = "the seam's six handles, the paste's meter and the injected wheel rule; \
              bundling them would add a per-event struct on the keystroke path"
)]
fn seam_egress_inner(
    term: &Mutex<Terminal>,
    modes: &ModeMirror,
    sink: &SinkWriter,
    ev: &InputEvent,
    mode: EgressMode,
    accepted_order: &mut Accepted,
    meter: Option<&BulkMeter>,
    platform: WheelPlatform,
) -> Egress {
    // Debug builds check that `modes` is this terminal's mirror (another's, or a `Default`,
    // would encode every key in legacy mode), only when the mutex is free: the mirror exists so
    // the seam never waits on it.
    #[cfg(debug_assertions)]
    if let Ok(t) = term.try_lock() {
        debug_assert!(
            std::ptr::eq(modes, &**t.mode_mirror()),
            "seam handed a ModeMirror that is not this terminal's (see mode_mirror_of)"
        );
    }
    match ev {
        InputEvent::Key {
            key,
            mods,
            base_layout,
            layout_text,
            event_type,
        } => {
            // Lock-free: `process()` republishes the mirror before every hold ends, so this is
            // the mode the last completed batch left.
            let bytes = aterm_types::keyboard::encode_key_with_text(
                key,
                *mods,
                modes.keyboard_mode(),
                *event_type,
                *base_layout,
                *layout_text,
            );
            let d = if bytes.is_empty() {
                Delivery::Full // faithful no-op (e.g. legacy release): nothing to deliver
            } else {
                emit(sink, mode, &bytes, accepted_order)
            };
            Egress::Reported(d)
        }
        InputEvent::Text(text) => {
            let mut d = Delivery::Full;
            if !text.is_empty() {
                // Lock-free like the Key arm: the mirror, not a mutex round-trip.
                let out = crate::keymap::encode_committed_text(text, modes.keyboard_mode());
                if !out.is_empty() {
                    d = emit(sink, mode, &out, accepted_order);
                }
            }
            Egress::Reported(d)
        }
        InputEvent::KeySequence(bytes) => {
            // Raw bytes go to the PTY verbatim: no keyboard-mode read, no encoder.
            let d = if bytes.is_empty() {
                Delivery::Full
            } else {
                emit(sink, mode, bytes, accepted_order)
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
                let mut t = term_lock(term);
                if t.mouse_tracking_enabled() {
                    // DEC 1016 reports a pixel coordinate, every other encoding the cell.
                    let (rx, ry) = report_coords(&t, *col, *row, *px_off);
                    let bytes = if *pressed {
                        t.encode_mouse_press(button.code(), rx, ry, *mods)
                    } else {
                        t.encode_mouse_release(button.code(), rx, ry, *mods)
                    };
                    // A press or release names a position too: xterm sets its
                    // motion memo on both, so a drag that never leaves the
                    // pressed cell reports no motion at all.
                    if bytes.is_some() {
                        t.note_mouse_report(rx, ry);
                    }
                    Some(bytes)
                } else {
                    None
                }
            };
            match report {
                Some(bytes) => {
                    let d = match bytes {
                        Some(b) => emit(sink, mode, &b, accepted_order),
                        None => Delivery::Full,
                    };
                    Egress::Reported(d)
                }
                None => Egress::TrackingOff {
                    wheel_lines: 0,
                    wheel_up: false,
                    viewport: None,
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
                let mut t = term_lock(term);
                if t.mouse_tracking_enabled() {
                    let (rx, ry) = report_coords(&t, *col, *row, *px_off);
                    // One report per position (xterm: button-event motion is reported only when
                    // the pointer moves to another cell), compared in the report's coordinates,
                    // so under DEC 1016 every new pixel is new. Both sources dedupe against the
                    // one memo every press, release, wheel and motion report writes.
                    if t.mouse_motion_repeats_last_report(rx, ry) {
                        Some(None)
                    } else {
                        let bytes = t.encode_mouse_motion(*buttons, rx, ry, *mods);
                        if bytes.is_some() {
                            t.note_mouse_report(rx, ry);
                        }
                        Some(bytes)
                    }
                } else {
                    None
                }
            };
            match report {
                Some(bytes) => {
                    let d = match bytes {
                        Some(b) => emit(sink, mode, &b, accepted_order),
                        None => Delivery::Full,
                    };
                    Egress::Reported(d)
                }
                None => Egress::TrackingOff {
                    wheel_lines: 0,
                    wheel_up: false,
                    viewport: None,
                },
            }
        }
        InputEvent::Wheel {
            dir,
            lines,
            detent,
            row,
            col,
            mods,
            px_off,
        } => {
            // Clamp `lines` to `1..=MAX_WHEEL_BURST` here, where every caller converges: a
            // non-positive count cannot emit zero reports for one source and N for another, and
            // one event cannot flood the PTY.
            let lines = (*lines).clamp(1, MAX_WHEEL_BURST);
            // Decided under one term_lock, source-blind:
            //   Write    — emit `bytes` `repeat` times: one wheel report per line under
            //              tracking, or (alt screen + DEC 1007) an arrow key scaled by the
            //              platform's lines-per-detent;
            //   Swallow  — reported with nothing to write (X10 mode 9 is press-only, or an
            //              empty key encoding): the local viewport must not move;
            //   Fallback — `App::input` scrolls the local viewport (`Egress::TrackingOff`), by
            //              zero lines on the horizontal axis.
            enum WheelPlan {
                Write {
                    bytes: Vec<u8>,
                    repeat: i32,
                },
                Swallow,
                /// With the viewport facts read under this lock, so the local scroll never
                /// re-locks for them.
                Fallback(WheelViewport),
            }
            let plan = {
                let mut t = term_lock(term);
                // The precedence lives in `wheel_route`; the facts are read here, under this
                // lock.
                let route = wheel_route_for(&t, *mods);
                if route == WheelRoute::Report {
                    let (rx, ry) = report_coords(&t, *col, *row, *px_off);
                    match t.encode_mouse_wheel(*dir, rx, ry, *mods) {
                        // One report per line, never scaled by lines-per-detent: the app does
                        // its own notch-to-lines conversion (vim's `mousescroll`), so scaling
                        // here would give nine lines a detent on Windows.
                        Some(b) => {
                            // A wheel notch is a press (xterm buttons 4-7): it names
                            // the pointer's position like any other.
                            t.note_mouse_report(rx, ry);
                            WheelPlan::Write {
                                bytes: b,
                                repeat: lines,
                            }
                        }
                        None => WheelPlan::Swallow, // X10 (mode 9) is press-only
                    }
                } else if route == WheelRoute::AltScroll
                    && let Some(bytes) = alt_scroll_arrow(*dir, t.keyboard_mode())
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
                    // distance the local viewport gets (`wheel_platform_lines`:
                    // `SPI_GETWHEELSCROLLLINES`, default 3, on Windows; VTE's
                    // ceil(rows/10) on Linux; the identity on macOS and for a precise
                    // row). xterm, VTE and Windows Terminal all send N arrows here.
                    // Unlike the tracking arm above there is nothing downstream to
                    // multiply again: `less` moves one line per ArrowDown, full
                    // stop. Bounded by [`MAX_WHEEL_BURST`] for
                    // the reason the `mouse` verb is: "One screen at a time"
                    // (WHEEL_PAGESCROLL) times a large flick would otherwise let one
                    // event flood the PTY.
                    //
                    // VERTICAL ONLY — `alt_scroll_arrow`'s `vertical_up()`. REJECTED:
                    // synthesizing Left/Right arrows for a horizontal flick. DEC 1007's
                    // whole premise is that the alt screen has no scrollback, so the
                    // wheel owes the PAGER the motion it would have made; a pager's
                    // left/right arrows are not a horizontal scroll — in `less` they are
                    // (by default) unbound-ish/positional, and in a shell's alt-screen
                    // TUI an ArrowRight is a CURSOR MOVE or a menu entry. xterm does not
                    // do it either: its alternateScroll only maps the vertical pair. A
                    // horizontal flick over a non-tracking alt-screen app is therefore
                    // nothing, which is exactly what it was before this widening.
                    if bytes.is_empty() {
                        WheelPlan::Swallow
                    } else {
                        WheelPlan::Write {
                            bytes,
                            repeat: alt_scroll_repeat(platform, lines, t.rows(), *detent),
                        }
                    }
                } else {
                    WheelPlan::Fallback(WheelViewport {
                        display_offset: t.grid().display_offset(),
                        scrollback_lines: t.grid().scrollback_lines(),
                        rows: t.rows(),
                    })
                }
            };
            match plan {
                WheelPlan::Write { bytes: b, repeat } => {
                    // One report or keypress per unit of `repeat`, computed source-blind.
                    let mut d = Delivery::Full;
                    for _ in 0..repeat {
                        let attempt = emit(sink, mode, &b, accepted_order);
                        if !attempt.is_full() {
                            d = attempt; // any short/failed write fails the lot
                        }
                    }
                    Egress::Reported(d)
                }
                WheelPlan::Swallow => Egress::Reported(Delivery::Full),
                // Vertical only: the grid has no horizontal viewport, so a tilt notch or a
                // horizontal swipe reports zero lines and `input_wheel` does nothing. Passing
                // `lines` would scroll a horizontal flick downward (`wheel_up` is false for it).
                WheelPlan::Fallback(viewport) => Egress::TrackingOff {
                    wheel_lines: if dir.is_horizontal() { 0 } else { lines },
                    wheel_up: dir.vertical_up().unwrap_or(false),
                    viewport: Some(viewport),
                },
            }
        }
        InputEvent::Paste(text, framing) => {
            // The event answers the framing question ([`PasteFraming`]): a paste reaches here on
            // the ordered-writer thread, arbitrarily later than its gesture. Both reads are
            // under this one lock.
            let out = {
                let t = term_lock(term);
                t.format_paste_framed(text, framing.bracketed(t.modes().bracketed_paste))
            };
            let d = match meter {
                Some(meter) if !out.is_empty() && mode == EgressMode::Backpressured => {
                    emit_metered(sink, &out, accepted_order, meter)
                }
                meter => {
                    let d = if out.is_empty() {
                        Delivery::Full
                    } else {
                        emit(sink, mode, &out, accepted_order)
                    };
                    // A meter this arm did not write through (the sanitizer left nothing to
                    // send, or a mode the metered write does not serve) is settled here, or its
                    // watch would never end.
                    if let Some(meter) = meter {
                        meter.settle_unmetered(d.is_full());
                    }
                    d
                }
            };
            Egress::Reported(d)
        }
        InputEvent::Focus(focused) => {
            // The sole focus-report egress: ESC[I / ESC[O under DEC 1004. Lock-free: this is
            // reached from `App::recompute_focus_reports` on every tab switch and pane-focus
            // move, and `App::sync_window` must never take the engine mutex (a flooding pane's
            // reader holds it for a whole `process()` slice).
            let mut d = Delivery::Full;
            if modes.focus_reporting_enabled() {
                let seq: &[u8] = if *focused { b"\x1b[I" } else { b"\x1b[O" };
                d = emit(sink, mode, seq, accepted_order);
            }
            Egress::Reported(d)
        }
        // No PTY bytes: `App::input` handles the viewport and geometry side effects.
        // `ResizeWindowPx` only asks the window for a size; the PTY learns it from `Resized`.
        InputEvent::ScrollView(_)
        | InputEvent::Resize { .. }
        | InputEvent::ResizeWindowPx { .. } => Egress::Reported(Delivery::Full),
    }
}

/// Escape a dropped file path so it survives as one shell argument at the prompt: POSIX shells
/// take backslashes ([`shell_escape_path_posix`]); on Windows the shell is pwsh/powershell/cmd,
/// where `\` is the path separator, so the path is double-quoted instead
/// ([`shell_escape_path_windows`]). No trailing space or newline: the drop site appends the one
/// separating space and routes the text through the paste seam, which never executes it.
pub(crate) fn shell_escape_path(path: &str) -> String {
    #[cfg(windows)]
    return shell_escape_path_windows(path);
    #[cfg(not(windows))]
    return shell_escape_path_posix(path);
}

/// Windows drop quoting: wrap the path in double quotes, the one form PowerShell and cmd share,
/// doubling any embedded `"` (defensive: NTFS forbids `"` in names). `\` passes through. A path
/// of only inert characters (alphanumerics, `\ / : . - _`) is returned unchanged; anything else
/// (space, `( )`, `,` (a PowerShell array separator), `& | < > ; @ $ ' ~` …) is quoted.
///
/// A name with characters live inside double quotes ([`live_in_double_quotes`]) goes in single
/// quotes instead ([`windows_single_quoted`]): whoever made the file chose the name, and the
/// dropped text runs when the person presses Enter. Known residue: cmd expands `%VAR%` in
/// either form (an environment value, not code).
#[cfg(any(windows, test))]
pub(crate) fn shell_escape_path_windows(path: &str) -> String {
    let inert = |c: char| c.is_alphanumeric() || matches!(c, '\\' | '/' | ':' | '.' | '-' | '_');
    if path.chars().all(inert) {
        return path.to_string();
    }
    if live_in_double_quotes(path) {
        return windows_single_quoted(path);
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

/// Whether `path` holds a character a shell aterm runs on Windows acts on inside double quotes:
/// PowerShell and Git Bash expand `$name`, `${…}` and `$(…)` (which runs a command) and read a
/// backtick (PowerShell's escape, bash's command substitution), and PowerShell ends a
/// double-quoted string at a typographic `“ ” „` as at `"`. A `$` before a separator or at the
/// end starts nothing (an admin share's `\\host\C$\`), so it stays double-quoted.
#[cfg(any(windows, test))]
fn live_in_double_quotes(path: &str) -> bool {
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '`' | '\u{201C}' | '\u{201D}' | '\u{201E}' => return true,
            '$' if !matches!(chars.peek(), None | Some('\\')) => return true,
            _ => {}
        }
    }
    false
}

/// The single-quoted form for a name [`live_in_double_quotes`] refuses.
/// PowerShell reads it literally once every character it takes for a single
/// quote (`'` and `‘ ’ ‚ ‛`) is doubled, its own escape; Git Bash reads it
/// literally. cmd has no single quotes, so there `^` goes before its command
/// separator `&` and before `^` itself, and the token cannot become a second
/// command. Such a name then reaches cmd and bash with extra characters and
/// names no file: no quoting serves it in every shell, and a wrong path is the
/// failure that runs nothing.
#[cfg(any(windows, test))]
fn windows_single_quoted(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 2);
    out.push('\'');
    for c in path.chars() {
        match c {
            '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => out.push(c),
            '&' | '^' => out.push('^'),
            _ => {}
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// POSIX drop escaping: backslash-escape the path, byte-for-byte what iTerm2 does on a file drop
/// (`+[NSString shellEscapableCharacters]` in `NSStringITerm.m`, plus the CR/LF of
/// `-stringWithEscapedShellCharactersIncludingNewlines:YES`), so no listed byte can split the
/// path or run as code. `\` is in the set; one forward pass reads only input characters, so it
/// cannot double-escape. Everything else (`/ . - _ , % :`, letters, digits, multibyte UTF-8) is
/// shell-inert and passes through.
#[cfg(any(unix, test))]
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

/// The terminal insertion for a list of file paths (the Windows `CF_HDROP` clipboard paste in
/// `App::paste_clipboard_into`), on the drag-and-drop contract (`App::drop_file`): each path
/// shell-escaped and followed by one space, never a newline, so nothing is executed; empty
/// paths contribute nothing.
#[cfg(any(windows, test))]
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

    /// A live PTY-master stand-in whose bytes the test reads back: the harness every byte-level
    /// test here drives [`seam_egress`] against, on every platform.
    ///
    /// * POSIX: `pipe(2)`; the `SinkWriter`'s master is the write-end fd.
    /// * Windows: a `SinkWriter`'s master is a key into aterm-pty's ConPTY session registry, so
    ///   the capture registers a session whose input handle is a pipe the test reads
    ///   ([`aterm_pty::adopt_handoff`]), driving the real `write_frame` path and `WriteFile`.
    ///
    /// The remaining `#[cfg(unix)]` tests assert POSIX-only facts (alternate-scroll arrow
    /// counts, which Windows multiplies by `SPI_GETWHEELSCROLLLINES`; backslash escaping).
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

    /// See the POSIX twin's docs. On Windows the capture is an anonymous pipe registered as a
    /// ConPTY session's input handle, so `SinkWriter::write_frame` reaches it through the
    /// ordinary `aterm_pty::write_some` path.
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
        /// Pipe buffer, far above the default: the harness reads only after `seam_egress`
        /// returns, and a burst past a 4 KiB buffer with no reader would block `WriteFile`
        /// forever. The largest burst (512 reports of ten bytes, ~5 KiB) has ~50x headroom.
        const PIPE_BYTES: u32 = 256 * 1024;

        fn new() -> Self {
            let (mut read, mut write): (isize, isize) = (0, 0);
            // SAFETY: two out-params, default security attributes, explicit size.
            let ok = unsafe {
                winapi::CreatePipe(
                    &mut read,
                    &mut write,
                    std::ptr::null_mut(),
                    Self::PIPE_BYTES,
                )
            };
            assert_ne!(ok, 0, "CreatePipe for the egress capture");
            // One event object under two handles, via the name: the session takes one as its
            // `client_process` (`adopt_handoff` refuses a null one) and `drop` signals the other
            // to wake its waiter. The name is unique per capture so concurrent tests never share.
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let nth = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let name: Vec<u16> = format!("aterm-egress-capture-{}-{nth}\0", std::process::id())
                .encode_utf16()
                .collect();
            // SAFETY: NULL attrs; manual-reset, initially unsignaled; NUL-terminated wide name.
            // The second call opens the same object (`CreateEventW` on an existing name).
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

        /// Every byte the sink has written. `PeekNamedPipe` first: the session's write end is
        /// still open, so a read to EOF would block forever, and the count already in the pipe
        /// is exact because Windows `write_frame` is a synchronous `WriteFile` that returned
        /// before `seam_egress` did.
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
            pub(crate) fn CreatePipe(
                read: *mut isize,
                write: *mut isize,
                attrs: *mut c_void,
                size: u32,
            ) -> i32;
            pub(crate) fn CreateEventW(
                attrs: *mut c_void,
                manual_reset: i32,
                initial_state: i32,
                name: *const u16,
            ) -> isize;
            pub(crate) fn SetEvent(event: isize) -> i32;
            pub(crate) fn CloseHandle(handle: isize) -> i32;
            pub(crate) fn PeekNamedPipe(
                pipe: isize,
                buf: *mut u8,
                buf_len: u32,
                read: *mut u32,
                avail: *mut u32,
                left_this_message: *mut u32,
            ) -> i32;
            pub(crate) fn ReadFile(
                file: isize,
                buf: *mut u8,
                want: u32,
                got: *mut u32,
                overlapped: *mut c_void,
            ) -> i32;
        }
    }

    /// Drive one [`InputEvent`] through [`seam_egress`] against a real [`SinkWriter`] over a
    /// readable master, and return the exact bytes that reached the "PTY".
    fn egress_bytes(term: &Mutex<Terminal>, ev: &InputEvent) -> Vec<u8> {
        let mut cap = CaptureSink::new();
        let sink = SinkWriter::new(cap.master());
        seam_egress(
            term,
            &crate::mode_mirror_of(term),
            &sink,
            ev,
            EgressMode::Interactive,
        );
        drop(sink);
        cap.drain()
    }

    /// Only a whole write is `Full`; a short write (peer closed mid-frame) or an error is `Failed`.
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

    #[test]
    fn receipt_tracks_accepted_bytes_not_zero_byte_or_failed_attempts() {
        let term = term_with(&[]);
        let mut cap = CaptureSink::new();
        let sink = SinkWriter::new(cap.master());
        let accepted = seam_egress_receipt(
            &term,
            &crate::mode_mirror_of(&term),
            &sink,
            &InputEvent::KeySequence(b"x".to_vec()),
            EgressMode::Interactive,
        );
        assert!(accepted.accepted_nonempty());
        assert!(accepted.fully_accepted_nonempty());
        drop(sink);
        assert_eq!(cap.drain(), b"x");

        let empty = seam_egress_receipt(
            &term,
            &crate::mode_mirror_of(&term),
            &SinkWriter::new(-1),
            &InputEvent::KeySequence(Vec::new()),
            EgressMode::Interactive,
        );
        assert_eq!(empty.egress, Egress::Reported(Delivery::Full));
        assert!(!empty.accepted_nonempty());
        assert!(!empty.fully_accepted_nonempty());

        let failed = seam_egress_receipt(
            &term,
            &crate::mode_mirror_of(&term),
            &SinkWriter::new(-1),
            &InputEvent::KeySequence(b"x".to_vec()),
            EgressMode::Interactive,
        );
        assert_eq!(failed.egress, Egress::Reported(Delivery::Failed));
        assert!(!failed.accepted_nonempty());
        assert!(!failed.fully_accepted_nonempty());
    }

    #[test]
    fn receipt_preserves_an_earlier_accepted_frame_after_a_later_failure() {
        let mut cap = CaptureSink::new();
        let good = SinkWriter::new(cap.master());
        let bad = SinkWriter::new(-1);
        let mut accepted = Accepted::default();
        assert_eq!(
            emit(&good, EgressMode::Interactive, b"first", &mut accepted),
            Delivery::Full,
        );
        assert!(accepted.direct, "an uncontended whole frame is direct");
        assert_eq!(
            emit(&bad, EgressMode::Interactive, b"second", &mut accepted),
            Delivery::Failed,
        );
        assert!(accepted.order.is_some());
        assert!(
            accepted.direct,
            "a later failed frame accepts nothing and leaves the earlier direct fact alone"
        );
        assert!(
            !accepted.refused,
            "a dead fd is a failure, not a full queue"
        );
        let receipt = EgressReceipt {
            egress: Egress::Reported(Delivery::Failed),
            accepted_order: accepted.order,
            direct: accepted.direct,
            refused: accepted.refused,
            queued: false,
        };
        assert!(receipt.accepted_nonempty());
        assert!(receipt.is_direct());
        assert!(
            !receipt.fully_accepted_nonempty(),
            "an earlier accepted frame cannot authorize a failed queued key"
        );
        drop(good);
        assert_eq!(cap.drain(), b"first");
    }

    /// A failed PTY write reports `Delivery::Failed` (`ERR write failed`), never a false OK; an
    /// invalid fd fails every write deterministically (EBADF, no SIGPIPE). A legacy key release
    /// encodes to nothing and stays `Full`.
    #[test]
    fn failed_pty_write_is_reported_not_falsely_ok() {
        use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
        let term = term_with(&[]);
        let sink = SinkWriter::new(-1); // invalid fd -> every write_frame errors
        let press = InputEvent::Key {
            key: Key::Named(NamedKey::ArrowUp),
            mods: Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        };
        assert_eq!(
            seam_egress(
                &term,
                &crate::mode_mirror_of(&term),
                &sink,
                &press,
                EgressMode::Interactive
            ),
            Egress::Reported(Delivery::Failed)
        );

        let release = InputEvent::Key {
            key: Key::Named(NamedKey::ArrowUp),
            mods: Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Release, // legacy: encodes to nothing
        };
        assert_eq!(
            seam_egress(
                &term,
                &crate::mode_mirror_of(&term),
                &sink,
                &release,
                EgressMode::Interactive
            ),
            Egress::Reported(Delivery::Full)
        );
    }

    /// `[key_sequences]` bytes reach the PTY verbatim even under a mode that would re-encode a
    /// real key; empty bytes are a `Full` no-op and a failed write is `Failed`.
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
                &crate::mode_mirror_of(&term),
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

    /// The Key and Text arms encode against the lock-free mode mirror: with the terminal mutex
    /// held for the whole call (a reader mid-`process()`), the egress still completes,
    /// byte-exact for the kitty mode negotiated before the hold.
    #[test]
    fn key_and_text_egress_complete_while_the_terminal_mutex_is_held() {
        use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
        let term = term_with(&[b"\x1b[>1u"]); // kitty disambiguate: a non-trivial mode
        let modes = crate::mode_mirror_of(&term);
        let mut cap = CaptureSink::new();
        let sink = SinkWriter::new(cap.master());
        let held = term_lock(&term); // the reader's hold — kept for the whole egress
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = {
            let term = term.clone();
            std::thread::spawn(move || {
                let press = InputEvent::Key {
                    key: Key::Named(NamedKey::Enter),
                    mods: Modifiers::SHIFT,
                    base_layout: None,
                    layout_text: None,
                    event_type: KeyEventType::Press,
                };
                let a = seam_egress(&term, &modes, &sink, &press, EgressMode::Interactive);
                let text = InputEvent::Text("é".to_string());
                let b = seam_egress(&term, &modes, &sink, &text, EgressMode::Interactive);
                drop(sink);
                let _ = tx.send((a, b));
            })
        };
        // A hang detector, not a latency budget: `held` drops only after this answer.
        let (a, b) = rx
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("the seam parked behind the terminal mutex to read keyboard_mode()");
        drop(held);
        worker.join().unwrap();
        assert_eq!(a, Egress::Reported(Delivery::Full));
        assert_eq!(b, Egress::Reported(Delivery::Full));
        // Byte-exact against the negotiated mode: Shift+Enter under disambiguate is
        // the CSI-u report, and the committed text follows it verbatim.
        assert_eq!(cap.drain(), b"\x1b[13;2u\xc3\xa9".to_vec());
    }

    /// A wheel event at the cell origin (row 0, col 0), no modifiers.
    fn wheel(dir: WheelDir, lines: i32) -> InputEvent {
        InputEvent::Wheel {
            dir,
            lines,
            detent: true,
            row: 0,
            col: 0,
            mods: 0,
            px_off: PixelOffset::CELL_ORIGIN,
        }
    }

    /// `arrow` once per line `notches` wheel detents owe a pager on [`term_with`]'s
    /// 24 rows ([`wheel_platform_lines`]): one on macOS, VTE's three on Linux.
    #[cfg(unix)]
    fn detent_arrows(arrow: &[u8], notches: i32) -> Vec<u8> {
        let lines = wheel_platform_lines(WheelPlatform::Host, notches, 24, true);
        arrow.repeat(usize::try_from(lines).expect("a non-negative count"))
    }

    /// Drive one event through [`seam_egress`] and return the [`Egress`] verdict, discarding any
    /// PTY bytes.
    fn egress_of(term: &Mutex<Terminal>, ev: &InputEvent) -> Egress {
        let cap = CaptureSink::new();
        let sink = SinkWriter::new(cap.master());
        seam_egress(
            term,
            &crate::mode_mirror_of(term),
            &sink,
            ev,
            EgressMode::Interactive,
        )
    }

    /// SELECTION CUSTODY Phase 2 — the LOCAL-SCROLL OVERRIDE, on EVERY platform.
    ///
    /// Through [`wheel_route_for`] — the engine reads plus the policy table, no
    /// sink at all — so the override the design calls load-bearing ("without this,
    /// Phase 4 is unreachable under any mouse-owning TUI") stays pinned even for a
    /// target with no PTY of any kind. With Option held the wheel scrolls this
    /// terminal over a tracking app on the MAIN screen; the alt screen (zero
    /// scrollback) keeps its report and its DEC-1007 arrows.
    #[test]
    fn the_option_wheel_override_routes_locally_on_every_platform() {
        use aterm_types::mouse::{ALT_MASK, SHIFT_MASK};

        let route = |term: &Mutex<Terminal>, mods: u8| {
            let t = term.lock().expect("terminal");
            super::wheel_route_for(&t, mods)
        };

        // Main screen, app grabbing the mouse (SGR tracking).
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

        // Alt screen: no override. There is no scrollback there, and taking the wheel would
        // cost the DEC 1007 arrows less/man/git-log scroll by.
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

        // No tracking, no alt screen: the wheel is local with or without Option.
        let plain = term_with(&[]);
        assert_eq!(route(&plain, 0), WheelRoute::Viewport);
        assert_eq!(route(&plain, ALT_MASK), WheelRoute::Viewport);

        // Alternate scroll applies only on the ALT screen, and only while the app
        // leaves DEC 1007 on: otherwise the wheel is the local viewport's.
        let main_1007 = term_with(&[b"\x1b[?1007h"]);
        assert_eq!(route(&main_1007, 0), WheelRoute::Viewport);
        let alt_1007_off = term_with(&[b"\x1b[?1049h", b"\x1b[?1007l"]);
        assert_eq!(route(&alt_1007_off, 0), WheelRoute::Viewport);
    }

    /// The lock-free route is the locked one for every combination of tracking, the alternate
    /// screen, DEC 1007 and the Shift/Option bypasses.
    #[test]
    fn the_mirrored_wheel_route_equals_the_locked_one_for_every_mode_combination() {
        use aterm_types::mouse::{ALT_MASK, SHIFT_MASK};

        let mut routes = std::collections::BTreeSet::new();
        for tracking in [false, true] {
            for alt_screen in [false, true] {
                for alt_scroll in [false, true] {
                    let mut seqs: Vec<&[u8]> = vec![if alt_scroll {
                        b"\x1b[?1007h"
                    } else {
                        b"\x1b[?1007l"
                    }];
                    if alt_screen {
                        seqs.push(b"\x1b[?1049h");
                    }
                    if tracking {
                        seqs.push(b"\x1b[?1000h\x1b[?1006h");
                    }
                    let term = term_with(&seqs);
                    let t = term.lock().expect("terminal");
                    for mods in [0, SHIFT_MASK, ALT_MASK, SHIFT_MASK | ALT_MASK] {
                        let locked = super::wheel_route_for(&t, mods);
                        assert_eq!(
                            super::wheel_route_mirrored(t.mode_mirror(), mods),
                            locked,
                            "tracking={tracking} alt_screen={alt_screen} \
                             alt_scroll={alt_scroll} mods={mods:#x}"
                        );
                        routes.insert(format!("{locked:?}"));
                    }
                }
            }
        }
        assert_eq!(
            routes.len(),
            3,
            "non-vacuous: the sweep reached every route ({routes:?})"
        );
    }

    /// Audit M5 — alternate scroll (DEC 1007): on the ALT screen with 1007 set and
    /// no mouse tracking, a wheel becomes arrow-key PRESSES (a detent's worth per
    /// notch, [`detent_arrows`]), encoded through the LIVE keyboard mode (SS3 under
    /// DECCKM), and the local viewport is left alone (`Egress::Reported`, not
    /// `TrackingOff`).
    ///
    /// STILL `#[cfg(unix)]`: every assertion here counts ARROWS, and Windows
    /// multiplies that count by the live `SPI_GETWHEELSCROLLLINES` — an
    /// unassertable machine setting. See
    /// `wheel_scale_is_the_platform_distance_not_one_line` for the Windows half.
    #[cfg(unix)]
    #[test]
    fn alt_screen_alternate_scroll_converts_wheel_to_arrows() {
        let term = term_with(&[b"\x1b[?1007h", b"\x1b[?1049h"]);
        assert_eq!(
            egress_bytes(&term, &wheel(WheelDir::Up, 2)),
            detent_arrows(b"\x1b[A", 2)
        );
        assert_eq!(
            egress_bytes(&term, &wheel(WheelDir::Down, 1)),
            detent_arrows(b"\x1b[B", 1)
        );
        // DECCKM (?1h): arrows switch to the SS3 form, so the bytes come from the live
        // keyboard mode.
        term.lock().unwrap().process(b"\x1b[?1h");
        assert_eq!(
            egress_bytes(&term, &wheel(WheelDir::Up, 1)),
            detent_arrows(b"\x1bOA", 1)
        );
        // The wheel is consumed as a key report: the viewport does not scroll.
        assert!(matches!(
            egress_of(&term, &wheel(WheelDir::Up, 1)),
            Egress::Reported(_)
        ));
    }

    /// Over a tracking + alt-scroll terminal a Shift wheel falls back to the local viewport and
    /// writes nothing, so a shifted report can never leak to the app.
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
            detent: true,
            row: 0,
            col: 0,
            mods: aterm_types::mouse::SHIFT_MASK,
            px_off: PixelOffset::CELL_ORIGIN,
        };
        assert!(matches!(
            egress_of(&term, &shifted),
            Egress::TrackingOff {
                wheel_lines: 2,
                wheel_up: true,
                ..
            }
        ));
        assert!(egress_bytes(&term, &shifted).is_empty());
        // Control: the unshifted twin still reports.
        assert_eq!(
            egress_bytes(&term, &wheel(WheelDir::Up, 1)),
            b"\x1b[<64;1;1M"
        );
    }

    /// A tracking-off wheel carries the viewport facts read under the seam's one lock, matching
    /// the engine's own values (scrolled back, so `display_offset` is non-zero).
    #[test]
    fn a_viewport_wheel_carries_the_engine_facts_from_its_one_lock() {
        let term = term_with(&[]);
        {
            let mut t = term.lock().unwrap();
            for i in 0..40 {
                t.process(format!("line {i}\r\n").as_bytes());
            }
            t.scroll_display(3);
        }
        let expected = {
            let t = term.lock().unwrap();
            WheelViewport {
                display_offset: t.grid().display_offset(),
                scrollback_lines: t.grid().scrollback_lines(),
                rows: t.rows(),
            }
        };
        assert!(expected.display_offset == 3 && expected.scrollback_lines >= 16);
        assert_eq!(
            egress_of(&term, &wheel(WheelDir::Up, 2)),
            Egress::TrackingOff {
                wheel_lines: 2,
                wheel_up: true,
                viewport: Some(expected),
            }
        );
        // The button/move arms scroll nothing and carry nothing.
        let release = InputEvent::MouseButton {
            button: aterm_types::mouse::MouseButton::Left,
            pressed: false,
            row: 0,
            col: 0,
            mods: 0,
            click_count: 1,
            side: aterm_core::selection::SelectionSide::Left,
            block: false,
            suppress_copy_on_select: false,
            px_off: PixelOffset::CELL_ORIGIN,
        };
        assert!(matches!(
            egress_of(&term, &release),
            Egress::TrackingOff { viewport: None, .. }
        ));
    }

    /// A horizontal wheel is reported while an app tracks the mouse and moves nothing locally
    /// otherwise (`wheel_lines: 0`), with DEC 1007 armed or under the Shift bypass too.
    #[test]
    fn horizontal_wheel_reports_only_while_tracking() {
        let sink = SinkWriter::new(-1);
        for dir in [WheelDir::Left, WheelDir::Right] {
            let tracking = term_with(&[b"\x1b[?1000h", b"\x1b[?1006h"]);
            assert!(
                matches!(
                    seam_egress(
                        &tracking,
                        &crate::mode_mirror_of(&tracking),
                        &sink,
                        &wheel(dir, 2),
                        EgressMode::Interactive
                    ),
                    Egress::Reported(_)
                ),
                "{dir:?} must reach a tracking app"
            );
            let idle = term_with(&[]);
            assert!(
                matches!(
                    seam_egress(
                        &idle,
                        &crate::mode_mirror_of(&idle),
                        &sink,
                        &wheel(dir, 2),
                        EgressMode::Interactive
                    ),
                    Egress::TrackingOff {
                        wheel_lines: 0,
                        wheel_up: false,
                        ..
                    }
                ),
                "{dir:?} must move nothing locally"
            );
            // …and with the alt screen + DEC 1007: alternate scroll is vertical only.
            let pager = term_with(&[b"\x1b[?1049h", b"\x1b[?1007h"]);
            assert!(
                matches!(
                    seam_egress(
                        &pager,
                        &crate::mode_mirror_of(&pager),
                        &sink,
                        &wheel(dir, 2),
                        EgressMode::Interactive
                    ),
                    Egress::TrackingOff {
                        wheel_lines: 0,
                        wheel_up: false,
                        ..
                    }
                ),
                "{dir:?} must not synthesize arrows for a pager"
            );
            // …and under the Shift bypass: aterm has no horizontal viewport to take it.
            let shifted = InputEvent::Wheel {
                dir,
                lines: 2,
                detent: true,
                row: 0,
                col: 0,
                mods: aterm_types::mouse::SHIFT_MASK,
                px_off: PixelOffset::CELL_ORIGIN,
            };
            let tracking = term_with(&[b"\x1b[?1000h", b"\x1b[?1006h"]);
            assert!(
                matches!(
                    seam_egress(
                        &tracking,
                        &crate::mode_mirror_of(&tracking),
                        &sink,
                        &shifted,
                        EgressMode::Interactive
                    ),
                    Egress::TrackingOff {
                        wheel_lines: 0,
                        wheel_up: false,
                        ..
                    }
                ),
                "shift+{dir:?} bypasses to a viewport that cannot pan"
            );
        }
        // Control: the vertical twin still carries its lines to the viewport.
        let idle = term_with(&[]);
        assert!(matches!(
            seam_egress(
                &idle,
                &crate::mode_mirror_of(&idle),
                &sink,
                &wheel(WheelDir::Up, 2),
                EgressMode::Interactive
            ),
            Egress::TrackingOff {
                wheel_lines: 2,
                wheel_up: true,
                ..
            }
        ));
    }

    /// One report per line up to [`MAX_WHEEL_BURST`] for every source; a non-positive count is
    /// floored at one.
    #[test]
    fn the_report_burst_is_clamped_at_both_ends_for_every_source() {
        let term = term_with(&[b"\x1b[?1000h", b"\x1b[?1006h"]);
        let one = b"\x1b[<64;1;1M";
        // One report per line, unscaled, below the ceiling.
        assert_eq!(
            egress_bytes(&term, &wheel(WheelDir::Up, 3)).len(),
            one.len() * 3
        );
        // At the ceiling, and just above it: `egress_bytes` reads its pipe after the write, so
        // an unclamped burst past the pipe buffer would hang instead of failing.
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

    /// THE WHEEL MULTIPLY ITSELF (Windows' arm, on every host: 6A-4). The
    /// live `SPI_GETWHEELSCROLLLINES` read cannot be asserted — its answer
    /// belongs to whoever's machine runs the suite — so the arithmetic every
    /// wheel gesture goes through is pinned here instead, on all three of the
    /// setting's shapes, and through [`WheelPlatform::Windows`] as the seam
    /// reads it. Both surfaces that scroll without an app's help route through
    /// this: the scrollback viewport (`App::input_wheel`) and the DEC-1007
    /// alt-scroll arrows.
    #[test]
    fn wheel_scale_is_the_platform_distance_not_one_line() {
        let windows = |notch| WheelPlatform::Windows(notch);
        assert_eq!(
            wheel_platform_lines(windows(WheelNotch::Lines(3)), 1, 24, true),
            3
        );
        assert_eq!(
            wheel_platform_lines(windows(WheelNotch::Page), 2, 50, true),
            100
        );
        assert_eq!(
            wheel_platform_lines(windows(WheelNotch::Lines(0)), 9, 24, true),
            0
        );
        // The Windows default: one detent is three lines, not winit's one `LineDelta`.
        assert_eq!(wheel_scaled_lines(1, WheelNotch::Lines(3), 24), 3);
        assert_eq!(wheel_scaled_lines(4, WheelNotch::Lines(3), 24), 12);
        // A slider at 1: one line per detent.
        assert_eq!(wheel_scaled_lines(2, WheelNotch::Lines(1), 24), 2);
        // "One screen at a time" is one viewport; a zero-row grid still moves.
        assert_eq!(wheel_scaled_lines(1, WheelNotch::Page, 24), 24);
        assert_eq!(wheel_scaled_lines(2, WheelNotch::Page, 50), 100);
        assert_eq!(wheel_scaled_lines(1, WheelNotch::Page, 0), 1);
        // "Wheel scrolling off" (0) is no motion, not clamped up to one line.
        assert_eq!(wheel_scaled_lines(9, WheelNotch::Lines(0), 24), 0);
        // Absurd values saturate instead of wrapping into a negative scroll.
        assert_eq!(
            wheel_scaled_lines(i32::MAX, WheelNotch::Lines(u32::MAX), 24),
            i32::MAX
        );
    }

    /// THE WHEEL MULTIPLY ITSELF (Linux and the BSDs, X11 and Wayland). Neither
    /// protocol has a lines-per-detent setting, so the desktop's distance is the
    /// one its terminal scrolls: VTE's `ceil(rows / 10)`, at least one. A wheel that
    /// moves one line a notch makes long scrollback three times slower to reach
    /// than in GNOME Terminal at the usual 24 rows. On every host, through
    /// [`WheelPlatform::Vte`] (6A-4).
    #[test]
    fn a_free_desktop_wheel_detent_is_vtes_distance() {
        let vte = WheelPlatform::Vte;
        // The 80x24 default: three lines, VTE's adjustment 377 -> 374 per notch.
        assert_eq!(vte_lines_per_detent(24), 3);
        assert_eq!(wheel_platform_lines(vte, 1, 24, true), 3);
        assert_eq!(wheel_platform_lines(vte, 4, 24, true), 12);
        // The ceiling, at its edges, and the floor of one for a short grid (and a
        // degenerate zero-row one, which must still move).
        assert_eq!(vte_lines_per_detent(20), 2);
        assert_eq!(vte_lines_per_detent(21), 3);
        assert_eq!(vte_lines_per_detent(50), 5);
        assert_eq!(vte_lines_per_detent(10), 1);
        assert_eq!(vte_lines_per_detent(1), 1);
        assert_eq!(vte_lines_per_detent(0), 1);
        assert_eq!(vte_lines_per_detent(u16::MAX), 6554);
        // Absurd counts saturate instead of wrapping into a negative scroll.
        assert_eq!(wheel_platform_lines(vte, i32::MAX, 24, true), i32::MAX);
        // macOS's delta is final.
        assert_eq!(wheel_platform_lines(WheelPlatform::Mac, 4, 24, true), 4);
    }

    /// [`alt_scroll_sends`] answers, with no page height, what
    /// [`alt_scroll_repeat`] answers at every height: whether a wheel sends a
    /// pager any arrow (`human_input::reaches_program` asks it from the mode
    /// mirror, which holds no height), on every platform's wheel rule.
    #[test]
    fn whether_alternate_scroll_sends_an_arrow_needs_no_page_height() {
        for platform in WheelPlatform::ARMS.into_iter().chain([WheelPlatform::Host]) {
            for rows in [0, 1, 10, 11, 24, 50, u16::MAX] {
                for lines in [1, 2, 7, MAX_WHEEL_BURST] {
                    for detent in [true, false] {
                        assert_eq!(
                            alt_scroll_repeat(platform, lines, rows, detent) > 0,
                            alt_scroll_sends(platform, lines, detent),
                            "{lines} line(s) at {rows} rows, detent={detent}, {platform:?}"
                        );
                    }
                }
            }
        }
    }

    /// A WHEEL REACHES THE PROGRAM EXACTLY WHERE THE SEAM SENDS IT BYTES (T1,
    /// T39). [`crate::human_input::reaches_program`] reads only the session's
    /// mode mirror; on every tracking mode, screen, DEC 1007 and DECCKM setting,
    /// direction, modifier and kind of delta below, it says a wheel reaches the
    /// program where this seam writes the program bytes, and nowhere else. Two
    /// cases the route alone got wrong are in the matrix, each named: an X10 (9)
    /// wheel takes the report route and the encoder swallows it (press-only),
    /// and a horizontal wheel on the alternate screen under DEC 1007 with no
    /// tracking takes the alternate-scroll route and sends no arrow (vertical
    /// only). NEGATIVE CONTROL (by scratch revert): "any route but the
    /// viewport" (the arm until 2026-10-18) counts both as reaching.
    #[test]
    fn a_wheel_reaches_the_program_exactly_where_the_seam_sends_it_bytes() {
        use aterm_types::mouse::{ALT_MASK, SHIFT_MASK};
        let modes: [&[&[u8]]; 10] = [
            &[],
            &[b"\x1b[?9h"],
            &[b"\x1b[?1000h"],
            &[b"\x1b[?1002h", b"\x1b[?1006h"],
            &[b"\x1b[?1003h"],
            &[b"\x1b[?1049h"],
            &[b"\x1b[?1049h", b"\x1b[?1007h"],
            &[b"\x1b[?1049h", b"\x1b[?1007h", b"\x1b[?1h"],
            &[b"\x1b[?1049h", b"\x1b[?1007h", b"\x1b[?9h"],
            &[b"\x1b[?1049h", b"\x1b[?1007h", b"\x1b[?1000h"],
        ];
        let x10 = String::from_utf8_lossy(b"\x1b[?9h").into_owned();
        let alt_scroll = String::from_utf8_lossy(b"\x1b[?1049h\x1b[?1007h").into_owned();
        let mut swallowed = Vec::new();
        for set in modes {
            let name = String::from_utf8_lossy(&set.concat()).into_owned();
            for dir in [
                WheelDir::Up,
                WheelDir::Down,
                WheelDir::Left,
                WheelDir::Right,
            ] {
                for mods in [0, SHIFT_MASK, ALT_MASK] {
                    for detent in [true, false] {
                        let term = term_with(set);
                        let ev = InputEvent::Wheel {
                            dir,
                            lines: 1,
                            detent,
                            row: 0,
                            col: 0,
                            mods,
                            px_off: PixelOffset::CELL_ORIGIN,
                        };
                        let route = wheel_route_for(&term.lock().unwrap(), mods);
                        let reaches = crate::human_input::reaches_program(
                            &ev,
                            &crate::mode_mirror_of(&term),
                            WheelPlatform::Host,
                        );
                        let sent = !egress_bytes(&term, &ev).is_empty();
                        assert_eq!(
                            reaches, sent,
                            "{name:?} {dir:?} mods={mods} detent={detent} on {route:?}"
                        );
                        if route != WheelRoute::Viewport && !sent && mods == 0 {
                            swallowed.push((name.clone(), dir, route));
                        }
                    }
                }
            }
        }
        for witness in [
            (x10, WheelDir::Up, WheelRoute::Report),
            (alt_scroll, WheelDir::Left, WheelRoute::AltScroll),
        ] {
            assert!(
                swallowed.contains(&witness),
                "{witness:?} is not swallowed: {swallowed:?}"
            );
        }
    }

    /// A precise device's row is a row on EVERY platform: a `PixelDelta` banked by
    /// the cell height already measured the finger's travel, so no lines-per-detent
    /// (VTE's, or Windows' live setting, which this never reads) multiplies it.
    #[test]
    fn a_precise_wheel_row_is_never_multiplied() {
        for platform in WheelPlatform::ARMS.into_iter().chain([WheelPlatform::Host]) {
            for rows in [0, 1, 24, 50, u16::MAX] {
                for lines in [0, 1, 2, 7, MAX_WHEEL_BURST] {
                    assert_eq!(wheel_platform_lines(platform, lines, rows, false), lines);
                }
            }
        }
    }

    /// The seam's two wheel surfaces on a free desktop, at the byte level: a pager
    /// under DEC 1007 gets a detent's three arrows at 24 rows (a precise row gets
    /// one), while a mouse-tracking app still gets exactly ONE report per notch, the
    /// app applying its own notch->lines conversion. The viewport fallback carries
    /// the notch count, unscaled, for `App::input_wheel` to scale. On every
    /// host: the pager's arrows through [`WheelPlatform::Vte`] (6A-4).
    #[test]
    fn a_free_desktop_detent_is_three_pager_arrows_but_one_tracking_report() {
        let vte = |term: &Mutex<Terminal>, ev: &InputEvent| {
            let sink = SinkWriter::new(-1);
            seam_encode_on(
                WheelPlatform::Vte,
                term,
                &crate::mode_mirror_of(term),
                &sink,
                ev,
            )
            .1
        };
        let precise = |dir| InputEvent::Wheel {
            dir,
            lines: 1,
            detent: false,
            row: 0,
            col: 0,
            mods: 0,
            px_off: PixelOffset::CELL_ORIGIN,
        };
        let pager = term_with(&[b"\x1b[?1007h", b"\x1b[?1049h"]);
        assert_eq!(
            vte(&pager, &wheel(WheelDir::Down, 1)),
            b"\x1b[B\x1b[B\x1b[B"
        );
        assert_eq!(vte(&pager, &wheel(WheelDir::Up, 2)), b"\x1b[A".repeat(6));
        assert_eq!(vte(&pager, &precise(WheelDir::Down)), b"\x1b[B");
        let tracking = term_with(&[b"\x1b[?1000h", b"\x1b[?1006h"]);
        assert_eq!(
            egress_bytes(&tracking, &wheel(WheelDir::Down, 1)),
            b"\x1b[<65;1;1M"
        );
        assert_eq!(
            egress_bytes(&tracking, &wheel(WheelDir::Up, 2)),
            b"\x1b[<64;1;1M".repeat(2)
        );
        assert!(matches!(
            egress_of(&term_with(&[]), &wheel(WheelDir::Up, 1)),
            Egress::TrackingOff {
                wheel_lines: 1,
                wheel_up: true,
                ..
            }
        ));
    }

    /// The Tier-1 indistinguishability invariant (A.7), part 2: the event a Human builds equals
    /// the one a Controller builds for the same intent. A winit `KeyEvent` cannot be constructed
    /// (`platform_specific` is `pub(crate)`), so the human side drives the primitives
    /// `keymap::build_key_input` uses (`aterm_winit_keymap::{map_logical_key,
    /// base_layout_key_for}`) and the controller side the real verb parsers (`parse_key`,
    /// `parse_ctrl`, `parse_mouse`).
    #[test]
    fn builders_converge() {
        use aterm_types::keyboard::{Key, Modifiers, NamedKey};
        use aterm_window::keyboard::{
            Key as WinitKey, KeyCode, NamedKey as WinitNamed, PhysicalKey,
        };
        use aterm_winit_keymap::{base_layout_key_for, map_logical_key};

        // --- "press the Up arrow" --------------------------------------------
        // Human: build_key_input's pure decision on a winit ArrowUp event.
        let human_up = {
            let key = map_logical_key(&WinitKey::Named(WinitNamed::ArrowUp)).expect("up maps");
            let base = base_layout_key_for(PhysicalKey::Code(KeyCode::ArrowUp));
            InputEvent::Key {
                key,
                mods: Modifiers::empty(),
                base_layout: base,
                layout_text: None,
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
                layout_text: None,
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
                layout_text: None,
                event_type: aterm_types::keyboard::KeyEventType::Press,
            }
        };
        let ctrl_ctrl_c = crate::control::parse_ctrl("c").expect("ctrl c parses");
        // Both encode Ctrl+c; the human path carries the physical `base_layout`, the
        // controller `None`. `base_layout` only adds the Kitty third field, which a plain `c`
        // leaves unchanged, so compare bytes, not structs.
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
        // Human-equivalent: the `on_mouse_input` builder's fields (count from the streak FSM,
        // block from Alt held).
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
            // scoped-edge policy is applied by the verb's arm AFTER parsing — so the
            // pure parser output stays struct-identical to the human builder.
            suppress_copy_on_select: false,
            // The verb has no real pointer: the cell origin on both sides.
            px_off: PixelOffset::CELL_ORIGIN,
        };
        assert_eq!(human_press, ctrl_press, "mouse-press builder mismatch");

        // --- "wheel up, 1 notch" ---------------------------------------------
        let ctrl_wheel = crate::control::parse_mouse("wheelup left 2 4").expect("wheelup parses");
        let human_wheel = InputEvent::Wheel {
            dir: WheelDir::Up,
            lines: 1,
            detent: true,
            row: 2,
            col: 4,
            mods: 0,
            px_off: PixelOffset::CELL_ORIGIN,
        };
        assert_eq!(human_wheel, ctrl_wheel, "wheel builder mismatch");
    }

    /// DEC 1016 (SGR-pixel) reports the pixel: with a 10×20 cell, a press in cell (col=3,row=2)
    /// at sub-cell offset (x=4,y=7) is pixel (34, 47), `ESC [ < 0 ; 35 ; 48 M` after
    /// `encode_sgr`'s +1.
    #[test]
    fn sgr_pixel_1016_reports_true_pixel_coordinate() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        {
            let mut t = term.lock().unwrap();
            t.set_cell_pixel_size(10, 20); // real font metrics the frontend reports
            t.process(b"\x1b[?1000h"); // Normal mouse tracking on
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

        // The same press under plain SGR (1006) reports the cell (col+1, row+1): the pixel
        // math is gated on 1016.
        let term_cell = term_with(&[b"\x1b[?1000h", b"\x1b[?1006h"]);
        assert_eq!(egress_bytes(&term_cell, &press), b"\x1b[<0;4;3M");
    }

    /// Jump-to-prompt navigates by prompts, not finished commands: an A/B-only shell (cmd.exe's
    /// `%PROMPT%`) leaves `command_marks()` empty (only OSC 133;D fills it), and its prompts are
    /// still reachable.
    #[test]
    fn jump_prompt_target_finds_prompts_from_an_a_b_only_shell() {
        let mut t = Terminal::new(4, 20);
        // Two A/B-only prompts (cmd.exe's PROMPT shape) with enough output between them to
        // build scrollback.
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

    /// A fully-marked shell's blocks and command marks carry the same `prompt_start_row`, so the
    /// union changes nothing for it.
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

    /// `report_coords` gives a pixel for 1016 and the cell for every other encoding, with the
    /// sub-cell offset clamped inside the cell.
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

    /// A plain path (only shell-inert bytes) is returned unchanged, as iTerm2 inserts it: dots,
    /// slashes, dashes, underscores, colons, commas, percent, digits and non-ASCII letters.
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

    /// Every character in iTerm2's drop escape set (`shellEscapableCharacters` plus CR/LF) is
    /// backslash-prefixed, and nothing else is.
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

    /// A filename crafted to break out of the argument is neutralised: every shell-significant
    /// byte in the output is preceded by its backslash.
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

    /// The framing field is the seam's only answer for a paste: `AtDrain` reads DEC 2004 now
    /// (the control verbs' write-time contract), and `Gesture` overrides it both ways, since the
    /// writer thread runs arbitrarily later than the gesture. Sanitizing and LF→CR are
    /// unaffected.
    #[test]
    fn the_seam_frames_a_paste_by_the_event_not_by_the_moment() {
        let live_off = term_with(&[]);
        let live_on = term_with(&[b"\x1b[?2004h"]);
        let paste = |framing| InputEvent::Paste("ls\nrm -rf ~".to_string(), framing);

        // AtDrain: the live mode.
        assert_eq!(
            egress_bytes(&live_off, &paste(PasteFraming::AtDrain)),
            b"ls\rrm -rf ~".to_vec()
        );
        assert_eq!(
            egress_bytes(&live_on, &paste(PasteFraming::AtDrain)),
            b"\x1b[200~ls\rrm -rf ~\x1b[201~".to_vec()
        );

        // Gesture: the captured answer wins over the mode now, so a program flipping 2004
        // under a queued paste changes nothing.
        assert_eq!(
            egress_bytes(&live_on, &paste(PasteFraming::Gesture { bracketed: false })),
            b"ls\rrm -rf ~".to_vec(),
            "2004 turned ON after the gesture cannot bracket an unbracketed promise"
        );
        assert_eq!(
            egress_bytes(&live_off, &paste(PasteFraming::Gesture { bracketed: true })),
            b"\x1b[200~ls\rrm -rf ~\x1b[201~".to_vec(),
            "2004 turned OFF after the gesture cannot unframe a paste judged inert"
        );
    }

    /// End to end through the paste seam: the drop site appends one space and pastes the escaped
    /// path; with DEC 2004 on it is wrapped in `ESC[200~ … ESC[201~`, as Cmd-V is. Unix only:
    /// the expected bytes are the POSIX form (`/p/My\ File`); `shell_escape_path_windows_*` pin
    /// the Windows form on every target.
    #[cfg(unix)]
    #[test]
    fn dropped_path_pastes_escaped_with_trailing_space() {
        let drop_text = |p: &str| {
            let mut t = shell_escape_path(p);
            t.push(' ');
            t
        };
        // Bracketed paste off: the escaped path and trailing space verbatim.
        let term = term_with(&[]);
        assert_eq!(
            egress_bytes(
                &term,
                &InputEvent::Paste(drop_text("/p/My File"), PasteFraming::AtDrain)
            ),
            b"/p/My\\ File ".to_vec()
        );
        // Bracketed paste on (DEC 2004): the same body inside the guards.
        let term = term_with(&[b"\x1b[?2004h"]);
        assert_eq!(
            egress_bytes(
                &term,
                &InputEvent::Paste(drop_text("/p/My File"), PasteFraming::AtDrain)
            ),
            b"\x1b[200~/p/My\\ File \x1b[201~".to_vec()
        );
    }

    /// Windows drop quoting passes a tame path through verbatim, backslashes never doubled (`\`
    /// is the separator, not an escape).
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

    /// A path under `Program Files` (embedded space) is double-quoted, one argument in
    /// PowerShell and cmd, with every backslash intact.
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

    /// PowerShell/cmd metacharacters beyond space (`,`, a PowerShell array separator;
    /// `& | ; @ $ ' ~`) trigger the quotes; an embedded `"` (impossible in NTFS names) is
    /// doubled, the escaped-quote form both shells share.
    #[test]
    fn shell_escape_path_windows_quotes_metachars_and_doubles_quotes() {
        assert_eq!(shell_escape_path_windows("C:\\a,b.txt"), "\"C:\\a,b.txt\"");
        assert_eq!(
            shell_escape_path_windows("C:\\a&b;c|d.txt"),
            "\"C:\\a&b;c|d.txt\""
        );
        assert_eq!(shell_escape_path_windows("a\"b"), "\"a\"\"b\"");
    }

    /// A file name is chosen by whoever made the file, and a dropped path runs when the person
    /// presses Enter. A name with an expansion live inside double quotes (`$(…)`, a backtick, a
    /// typographic `”`) goes in single quotes, PowerShell's single-quote class doubled and cmd's
    /// `&` and `^` caret-escaped.
    #[test]
    fn shell_escape_path_windows_never_leaves_an_expansion_live() {
        let dl = "C:\\Users\\me\\Downloads\\";
        for (name, want) in [
            ("a$(calc).pdf", "'a$(calc).pdf'"),
            ("a${env:PATH}.pdf", "'a${env:PATH}.pdf'"),
            ("a$x b.pdf", "'a$x b.pdf'"),
            ("a`$(calc)`.pdf", "'a`$(calc)`.pdf'"),
            (
                "a\u{201D};calc;\u{201D}.pdf",
                "'a\u{201D};calc;\u{201D}.pdf'",
            ),
            ("a\u{201C} b.pdf", "'a\u{201C} b.pdf'"),
            ("a\u{201E} b.pdf", "'a\u{201E} b.pdf'"),
            // The single-quote class is doubled, PowerShell's own escape.
            ("O'Neil $(x).pdf", "'O''Neil $(x).pdf'"),
            (
                "a\u{2019};calc;\u{2018}$(x).pdf",
                "'a\u{2019}\u{2019};calc;\u{2018}\u{2018}$(x).pdf'",
            ),
            // cmd reads `'` as a character: its separator stays escaped.
            ("a&calc&$(x).pdf", "'a^&calc^&$(x).pdf'"),
            ("a^&$(x).pdf", "'a^^^&$(x).pdf'"),
        ] {
            assert_eq!(
                shell_escape_path_windows(&format!("{dl}{name}")),
                want.replacen('\'', &format!("'{dl}"), 1),
                "{name:?}"
            );
        }
        // A `$` that starts nothing (an admin share's `C$\`, a trailing `$`)
        // keeps the double quotes cmd needs for a space.
        assert_eq!(
            shell_escape_path_windows("\\\\host\\C$\\My Docs\\a.txt"),
            "\"\\\\host\\C$\\My Docs\\a.txt\""
        );
        assert_eq!(shell_escape_path_windows("C:\\x y$"), "\"C:\\x y$\"");
    }

    /// The CF_HDROP paste insertion is the drop contract: per-path `shell_escape_path` plus one
    /// trailing space, never a newline, so nothing runs.
    #[test]
    fn paths_paste_insertion_matches_the_drop_contract() {
        let one = vec!["C:\\Users\\me\\a.txt".to_string()];
        assert_eq!(
            paths_paste_insertion(&one),
            format!("{} ", shell_escape_path("C:\\Users\\me\\a.txt"))
        );
        // Multi-file: iTerm's `p1 p2 p3 ` shape, each path with its own trailing space.
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

    /// The wheel decision table: Shift bypasses tracking and alt-scroll before either is
    /// tested, so a shifted wheel never reaches `encode_mouse_wheel` (whose `SHIFT_MASK` fold
    /// would leak a shifted report). Pure, so a precedence regression names its row.
    #[test]
    fn wheel_route_shift_bypasses_tracking_before_every_other_test() {
        use super::{WheelRoute, wheel_route};
        // Shift held: the viewport wins over every mode combination.
        for tracking in [false, true] {
            for alt_scroll in [false, true] {
                assert_eq!(
                    wheel_route(true, false, tracking, alt_scroll),
                    WheelRoute::Viewport,
                    "shift must bypass (tracking={tracking}, alt_scroll={alt_scroll})"
                );
            }
        }
        // Unshifted: tracking first, then DEC 1007 alt-scroll, then the local viewport.
        assert_eq!(wheel_route(false, false, true, false), WheelRoute::Report);
        assert_eq!(wheel_route(false, false, true, true), WheelRoute::Report);
        assert_eq!(
            wheel_route(false, false, false, true),
            WheelRoute::AltScroll
        );
        assert_eq!(
            wheel_route(false, false, false, false),
            WheelRoute::Viewport
        );
        // Alt on the main screen reaches local history over a tracking app. The caller sets
        // `alt_local` only off the alt screen, so `alt_scroll` is never true beside it in
        // practice; the table still answers every combination.
        assert_eq!(wheel_route(false, true, true, false), WheelRoute::Viewport);
        assert_eq!(wheel_route(false, true, false, false), WheelRoute::Viewport);
        assert_eq!(wheel_route(false, true, true, true), WheelRoute::Viewport);
        // Shift and Alt agree on the destination; either alone suffices.
        assert_eq!(wheel_route(true, true, true, true), WheelRoute::Viewport);
    }
}
