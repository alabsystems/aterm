// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Engine-neutral [`InputEvent`] vocabulary and [`Source`] audit tag for the input convergence seam.

use std::sync::Mutex;

use aterm_core::selection::SelectionSide;
use aterm_core::terminal::{ModeMirror, Terminal};
use aterm_session::sink::{AcceptedOrder, BulkMeter, SinkWriter, WriteReceipt, WriteReceiptError};
use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey, encode_key_with_event};
use aterm_types::mouse::MouseButton;

use crate::term_lock;

/// One logical input event, engine-neutral.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InputEvent {
    /// A key chord.
    Key {
        key: Key,
        mods: Modifiers,
        base_layout: Option<char>,
        /// The character the active layout typed for a text press.
        layout_text: Option<char>,
        /// Press / Repeat / Release.
        event_type: aterm_types::keyboard::KeyEventType,
    },
    /// Literal text to type: the `on_key` bare-`ev.text` fallback and the IME `Ime::Commit` path.
    Text(String),
    /// Raw bytes written to the PTY verbatim, never re-encoded.
    KeySequence(Vec<u8>),
    /// A mouse button press/release at a grid cell.
    MouseButton {
        button: MouseButton,
        pressed: bool,
        row: u16,
        col: u16,
        mods: u8,
        click_count: u8,
        side: SelectionSide,
        /// Single-click press starts a `Block` (rectangular) selection rather than `Simple`.
        block: bool,
        /// When `true`, a completed drag-select settled by THIS event's release must NOT auto-copy to the system clipboard / X11 PRIMARY.
        suppress_copy_on_select: bool,
        /// Sub-cell pixel offset inside the (`row`,`col`) cell, for DEC 1016 SGR-pixel mode.
        px_off: PixelOffset,
    },
    /// Pointer motion.
    MouseMove {
        buttons: u8,
        row: u16,
        col: u16,
        mods: u8,
        side: SelectionSide,
        /// Sub-cell pixel offset inside the (`row`,`col`) cell for DEC 1016.
        px_off: PixelOffset,
    },
    /// A wheel notch / trackpad flick of `lines` lines.
    Wheel {
        dir: aterm_types::mouse::WheelDir,
        lines: i32,
        /// Whether `lines` counts wheel DETENTS (winit's `LineDelta`: a notch, an X11 scroll-valuator step, the `mouse` verb) rather than rows a precise device already measured.
        detent: bool,
        row: u16,
        col: u16,
        mods: u8,
        /// Sub-cell pixel offset inside the (`row`,`col`) cell for DEC 1016.
        px_off: PixelOffset,
    },
    /// Explicit, tracking-agnostic scrollback navigation (the `scroll` verb).
    ScrollView(ScrollIntent),
    /// Paste text as if typed, framed per [`PasteFraming`].
    Paste(String, PasteFraming),
    /// A geometry change, re-clamped against `MAX_GRID_*` in the seam.
    Resize {
        rows: u16,
        cols: u16,
        echo_to_window: bool,
    },
    /// A window-pixel geometry change.
    ResizeWindowPx { width: u32, height: u32 },
    /// Focus gained/lost, for DEC 1004 focus reporting.
    Focus(bool),
}

impl InputEvent {
    /// This event with a keypad key folded onto its main-block twin (`Key::main_block_twin`).
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

/// The character a text press of base key `c` types.
#[must_use]
pub(crate) fn typed_glyph(c: char, mods: Modifiers, layout_text: Option<char>) -> char {
    layout_text.unwrap_or_else(|| aterm_types::keyboard::typed_character(c, mods))
}

/// Sub-cell pixel offset of the pointer inside its grid cell.
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

/// When the bracketed-paste (DEC 2004) question is answered for one [`InputEvent::Paste`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PasteFraming {
    /// Read DEC 2004 when the bytes are produced.
    AtDrain,
    /// Frame by `bracketed` whatever the mode says at drain time.
    Gesture { bracketed: bool },
}

impl PasteFraming {
    /// The framing for a paste being written now, given the terminal's live DEC 2004 state.
    #[must_use]
    pub(crate) const fn bracketed(self, live: bool) -> bool {
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
    /// Jump the viewport to the previous (older) recorded OSC 133 prompt row above the current top visible row.
    PrevPrompt,
    /// Jump the viewport to the next (newer) recorded prompt row below the current top visible row.
    NextPrompt,
}

/// The target absolute row for a prompt-to-prompt jump.
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

/// Who produced an [`InputEvent`].
#[derive(Clone, Copy, Debug)]
pub(crate) enum Source {
    /// An in-thread winit handler (real keyboard/mouse/focus on this window).
    Human,
    /// A control-socket verb, or input aterm sends on its own after a person's gesture.
    Controller,
}

/// The reply a reply-bearing verb gets back from the seam.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InputOutcome {
    /// Applied (bytes written and/or viewport moved).
    Ok,
    /// A `Resize` fell outside `1..=MAX_GRID_*` and was not applied.
    RangeRejected,
    /// The encoded bytes were not (fully) written to the PTY.
    WriteFailed,
    /// The session's input queue refused the event and NOTHING of it was written.
    QueueFull,
}

impl InputOutcome {
    /// Whether the event's bytes did not (all) reach the PTY.
    pub(crate) const fn failed(self) -> bool {
        matches!(self, Self::WriteFailed | Self::QueueFull)
    }
}

/// Whether [`seam_egress_receipt`] delivered the event's encoded bytes to the PTY.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// Every intended byte reached the PTY (or there were none to write).
    Full,
    /// A short or failed PTY write — the bytes did not (fully) land.
    Failed,
    /// The immediate actuator path accepted zero bytes and queued nothing.
    BusyZero,
    /// The immediate actuator path handed this prefix to the kernel and queued no tail.
    PartialInDoubt { accepted: usize },
}

impl Delivery {
    #[must_use]
    pub(crate) const fn is_full(self) -> bool {
        matches!(self, Self::Full)
    }
}

fn delivered(res: std::io::Result<usize>, intended: usize) -> Delivery {
    match res {
        Ok(n) if n == intended => Delivery::Full,
        _ => Delivery::Failed,
    }
}

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

/// What [`seam_egress_receipt`] did with a mouse/wheel event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Egress {
    /// The event produced a tracking report (or had no local fallback): the seam is done with it.
    Reported(Delivery),
    /// Mouse tracking is off: `App::input` runs the local fallback.
    TrackingOff {
        wheel_lines: i32,
        wheel_up: bool,
        viewport: Option<WheelViewport>,
    },
}

/// The engine facts a tracking-off wheel's local viewport scroll consumes, snapshotted under the seam's single `term_lock` when the route was decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WheelViewport {
    /// `Grid::display_offset()` — the viewport's current row into history.
    pub display_offset: usize,
    /// `Grid::scrollback_lines()` — the clamp bound for the glide's target.
    pub scrollback_lines: usize,
    /// `Terminal::rows()` — the page for Windows' "One screen at a time" detent.
    pub rows: u16,
}

/// One egress verdict plus the order of its last conclusively accepted non-empty PTY frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EgressReceipt {
    pub(crate) egress: Egress,
    accepted_order: Option<AcceptedOrder>,
    direct: bool,
    refused: bool,
    queued: bool,
}

impl EgressReceipt {
    /// Adapt a delivery reported by a guarded seam that performs the same sink operation but must keep its terminal lock across an authorization check.
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

    /// An event the seam decided and nothing wrote.
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

    /// FIFO admission is not PTY acceptance.
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

    /// The ordered writer refused the event.
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

    /// Whether the event was refused because its session's input queue is full (see the field).
    #[must_use]
    pub(crate) const fn input_queue_full(self) -> bool {
        self.refused
    }

    /// Whether the event was handed to the per-session ordered writer (see the field) rather than written, refused or dropped here.
    #[must_use]
    pub(crate) const fn is_queued(self) -> bool {
        self.queued
    }

    /// Whether at least one byte was conclusively accepted by the sink.
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

    /// The whole non-empty event was accepted.
    #[must_use]
    pub(crate) const fn fully_accepted_nonempty(self) -> bool {
        matches!(self.egress, Egress::Reported(Delivery::Full)) && self.accepted_order.is_some()
    }
}

/// The `(x, y)` a mouse report carries under `term`'s current encoding.
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

/// Ceiling on how many times one wheel event may write its encoded bytes.
pub(crate) const MAX_WHEEL_BURST: i32 = 512;

/// How far a wheel gesture of `lines` travels, in lines.
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

/// How far ONE wheel notch travels, per the user's Windows mouse settings.
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

/// THE PLATFORM'S WHEEL RULE.
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
    /// This host's own rule, read where a detent needs it.
    Host,
    /// macOS: AppKit folds the user's scroll speed into the delta before winit sees it.
    Mac,
    /// Linux and the BSDs: VTE's distance ([`vte_lines_per_detent`]).
    Vte,
    /// Windows, on the Mouse setting `notch` ([`wheel_scaled_lines`]).
    Windows(WheelNotch),
}

impl WheelPlatform {
    /// The lines `notch_lines` detents scroll a surface `page_rows` tall, by this rule.
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

    /// Every arm a test injects: macOS, the free desktops, and Windows on the default three lines, "wheel scrolling off" and "One screen at a time".
    #[cfg(test)]
    pub(crate) const ARMS: [WheelPlatform; 5] = [
        WheelPlatform::Mac,
        WheelPlatform::Vte,
        WheelPlatform::Windows(WheelNotch::Lines(3)),
        WheelPlatform::Windows(WheelNotch::Lines(0)),
        WheelPlatform::Windows(WheelNotch::Page),
    ];
}

#[cfg(windows)]
fn host_wheel_platform() -> WheelPlatform {
    WheelPlatform::Windows(crate::platform_win::wheel_notch_scroll())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn host_wheel_platform() -> WheelPlatform {
    WheelPlatform::Vte
}

#[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
const fn host_wheel_platform() -> WheelPlatform {
    WheelPlatform::Mac
}

fn vte_lines_per_detent(page_rows: u16) -> i32 {
    i32::from(page_rows.div_ceil(10)).max(1)
}

/// Where one wheel event goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WheelRoute {
    /// An app is tracking the mouse: encode wheel reports for it.
    Report,
    /// Alt screen + DEC 1007, tracking off: synthesize arrow-key presses.
    AltScroll,
    /// aterm's own scrollback viewport (`Egress::TrackingOff`).
    Viewport,
}

/// Resolve a wheel's [`WheelRoute`] from the facts the seam reads under its term lock.
pub(crate) const fn wheel_route(
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

/// The four facts [`wheel_route`] decides on, read from one engine under the caller's lock.
pub(crate) fn wheel_route_for(t: &Terminal, mods: u8) -> WheelRoute {
    wheel_route(
        mods & aterm_types::mouse::SHIFT_MASK != 0,
        mods & aterm_types::mouse::ALT_MASK != 0 && !t.is_alternate_screen(),
        t.mouse_tracking_enabled(),
        t.is_alternate_screen() && t.modes().alternate_scroll,
    )
}

/// [`wheel_route_for`]'s lock-free twin.
pub(crate) fn wheel_route_mirrored(mirror: &ModeMirror, mods: u8) -> WheelRoute {
    let alt_screen = mirror.alternate_screen();
    wheel_route(
        mods & aterm_types::mouse::SHIFT_MASK != 0,
        mods & aterm_types::mouse::ALT_MASK != 0 && !alt_screen,
        mirror.mouse_tracking_enabled(),
        alt_screen && mirror.alternate_scroll(),
    )
}

/// The arrow key alternate scroll (DEC 1007, `WheelRoute::AltScroll`) sends for one wheel line turned `dir`.
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

/// How many [`alt_scroll_arrow`] presses one wheel of `lines` (already clamped to `1..=MAX_WHEEL_BURST`) sends a pager showing `page_rows` rows.
pub(crate) fn alt_scroll_repeat(
    platform: WheelPlatform,
    lines: i32,
    page_rows: u16,
    detent: bool,
) -> i32 {
    wheel_platform_lines(platform, lines, page_rows, detent).clamp(0, MAX_WHEEL_BURST)
}

/// Whether one wheel of `lines` (as the event carries it; clamped here as the seam clamps it) sends a pager any [`alt_scroll_arrow`] press at all.
pub(crate) fn alt_scroll_sends(platform: WheelPlatform, lines: i32, detent: bool) -> bool {
    alt_scroll_repeat(platform, lines.clamp(1, MAX_WHEEL_BURST), 1, detent) > 0
}

fn wheel_scaled_lines(notch_lines: i32, notch: WheelNotch, page_rows: u16) -> i32 {
    let per_notch = match notch {
        WheelNotch::Lines(n) => i32::try_from(n).unwrap_or(i32::MAX),
        WheelNotch::Page => i32::from(page_rows).max(1),
    };
    notch_lines.saturating_mul(per_notch)
}

/// How [`seam_egress_receipt`] hands the encoded bytes to the [`SinkWriter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EgressMode {
    /// The UI event-loop thread, and a control socket lane writing into a session that is not on screen.
    Interactive,
    /// An expendable egress thread (the per-session egress-order writer).
    Backpressured,
    /// The UI thread where a write could block it (Windows: ConPTY has no non-blocking write), or behind a queued paste: produce exactly the bytes `Interactive` would and write nothing.
    Encode,
}

fn emit(sink: &SinkWriter, mode: EgressMode, bytes: &[u8], accepted: &mut Accepted) -> Delivery {
    debug_assert!(!bytes.is_empty());
    let (delivery, order, direct, refused) = match mode {
        EgressMode::Interactive => {
            crate::echo_rtt::note_write_start();
            delivered_receipt(
                sink.write_frame_interactive_with_receipt(bytes),
                bytes.len(),
            )
        }
        EgressMode::Backpressured => {
            crate::echo_rtt::note_write_start();
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

fn emit_metered(
    sink: &SinkWriter,
    bytes: &[u8],
    accepted: &mut Accepted,
    meter: &BulkMeter,
) -> Delivery {
    crate::echo_rtt::note_write_start();
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

#[derive(Clone, Debug, Default)]
struct Accepted {
    order: Option<AcceptedOrder>,
    direct: bool,
    refused: bool,
    encoded: Vec<u8>,
}

/// [`seam_egress_receipt`] without the receipt: the spelling the byte-level tests drive.
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

/// The source-blind byte-producing core of the seam (design A.2 / A.7).
pub(crate) fn seam_egress_receipt(
    term: &Mutex<Terminal>,
    modes: &ModeMirror,
    sink: &SinkWriter,
    ev: &InputEvent,
    mode: EgressMode,
) -> EgressReceipt {
    seam_egress_receipt_metered(term, modes, sink, ev, mode, None)
}

/// [`seam_egress_receipt`] with a [`BulkMeter`] for a large paste.
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

/// [`seam_egress_receipt`] under [`EgressMode::Encode`].
pub(crate) fn seam_encode(
    term: &Mutex<Terminal>,
    modes: &ModeMirror,
    sink: &SinkWriter,
    ev: &InputEvent,
) -> (Egress, Vec<u8>) {
    seam_encode_on(WheelPlatform::Host, term, modes, sink, ev)
}

/// [`seam_encode`] on the wheel rule `platform` ([`WheelPlatform`]).
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
            let bytes = aterm_types::keyboard::encode_key_with_text(
                key,
                *mods,
                modes.keyboard_mode(),
                *event_type,
                *base_layout,
                *layout_text,
            );
            let d = if bytes.is_empty() {
                Delivery::Full
            } else {
                emit(sink, mode, &bytes, accepted_order)
            };
            Egress::Reported(d)
        }
        InputEvent::Text(text) => {
            let mut d = Delivery::Full;
            if !text.is_empty() {
                let out = crate::keymap::encode_committed_text(text, modes.keyboard_mode());
                if !out.is_empty() {
                    d = emit(sink, mode, &out, accepted_order);
                }
            }
            Egress::Reported(d)
        }
        InputEvent::KeySequence(bytes) => {
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
                    let (rx, ry) = report_coords(&t, *col, *row, *px_off);
                    let bytes = if *pressed {
                        t.encode_mouse_press(button.code(), rx, ry, *mods)
                    } else {
                        t.encode_mouse_release(button.code(), rx, ry, *mods)
                    };
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
            let lines = (*lines).clamp(1, MAX_WHEEL_BURST);
            enum WheelPlan {
                Write { bytes: Vec<u8>, repeat: i32 },
                Swallow,
                Fallback(WheelViewport),
            }
            let plan = {
                let mut t = term_lock(term);
                let route = wheel_route_for(&t, *mods);
                if route == WheelRoute::Report {
                    let (rx, ry) = report_coords(&t, *col, *row, *px_off);
                    match t.encode_mouse_wheel(*dir, rx, ry, *mods) {
                        Some(b) => {
                            t.note_mouse_report(rx, ry);
                            WheelPlan::Write {
                                bytes: b,
                                repeat: lines,
                            }
                        }
                        None => WheelPlan::Swallow,
                    }
                } else if route == WheelRoute::AltScroll
                    && let Some(bytes) = alt_scroll_arrow(*dir, t.keyboard_mode())
                {
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
                    let mut d = Delivery::Full;
                    for _ in 0..repeat {
                        let attempt = emit(sink, mode, &b, accepted_order);
                        if !attempt.is_full() {
                            d = attempt;
                        }
                    }
                    Egress::Reported(d)
                }
                WheelPlan::Swallow => Egress::Reported(Delivery::Full),
                WheelPlan::Fallback(viewport) => Egress::TrackingOff {
                    wheel_lines: if dir.is_horizontal() { 0 } else { lines },
                    wheel_up: dir.vertical_up().unwrap_or(false),
                    viewport: Some(viewport),
                },
            }
        }
        InputEvent::Paste(text, framing) => {
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
                    if let Some(meter) = meter {
                        meter.settle_unmetered(d.is_full());
                    }
                    d
                }
            };
            Egress::Reported(d)
        }
        InputEvent::Focus(focused) => {
            let mut d = Delivery::Full;
            if modes.focus_reporting_enabled() {
                let seq: &[u8] = if *focused { b"\x1b[I" } else { b"\x1b[O" };
                d = emit(sink, mode, seq, accepted_order);
            }
            Egress::Reported(d)
        }
        InputEvent::ScrollView(_)
        | InputEvent::Resize { .. }
        | InputEvent::ResizeWindowPx { .. } => Egress::Reported(Delivery::Full),
    }
}

/// Escape a dropped file path so it survives as one shell argument at the prompt.
pub(crate) fn shell_escape_path(path: &str) -> String {
    #[cfg(windows)]
    return shell_escape_path_windows(path);
    #[cfg(not(windows))]
    return shell_escape_path_posix(path);
}

/// Windows drop quoting: an inert path as is; else double quotes (embedded `"` doubled), or single
/// quotes when something in it is live inside double quotes ([`live_in_double_quotes`]).
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

/// POSIX drop escaping: backslash-escape the path, byte-for-byte what iTerm2 does on a file drop.
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

/// The terminal insertion for a list of file paths (the Windows `CF_HDROP` clipboard paste in `App::paste_clipboard_into`), on the drag-and-drop contract (`App::drop_file`).
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

    #[cfg(unix)]
    struct CaptureSink {
        read: i32,
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

        fn master(&self) -> i32 {
            self.write
        }

        fn drain(&mut self) -> Vec<u8> {
            if self.write >= 0 {
                unsafe { libc::close(self.write) };
                self.write = -1;
            }
            let mut buf = Vec::new();
            let mut reader = unsafe { std::fs::File::from_raw_fd(self.read) };
            self.read = -1;
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

    #[cfg(windows)]
    struct CaptureSink {
        master: i32,
        read: isize,
        stop: isize,
    }

    #[cfg(windows)]
    impl CaptureSink {
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
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let nth = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let name: Vec<u16> = format!("aterm-egress-capture-{}-{nth}\0", std::process::id())
                .encode_utf16()
                .collect();
            // SAFETY: NULL attrs; manual-reset, initially unsignaled; NUL-terminated wide name. The second call opens the SAME object (documented `CreateEventW` behaviour for an existing name).
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
            // SAFETY: handles this struct owns, closed exactly once.
            unsafe { winapi::SetEvent(self.stop) };
            aterm_pty::close_master(self.master);
            unsafe {
                winapi::CloseHandle(self.stop);
                winapi::CloseHandle(self.read);
            }
        }
    }

    #[cfg(windows)]
    mod winapi {
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

    #[test]
    fn delivered_classifies_short_and_failed_writes() {
        assert_eq!(delivered(Ok(5), 5), Delivery::Full);
        assert_eq!(delivered(Ok(0), 5), Delivery::Failed);
        assert_eq!(delivered(Ok(3), 5), Delivery::Failed);
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

    #[test]
    fn failed_pty_write_is_reported_not_falsely_ok() {
        use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
        let term = term_with(&[]);
        let sink = SinkWriter::new(-1);
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
            event_type: KeyEventType::Release,
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

    #[test]
    fn key_sequence_egress_is_verbatim_and_reply_faithful() {
        let term = term_with(&[b"\x1b[>1u", b"\x1b[?1h"]);
        assert_eq!(
            egress_bytes(&term, &InputEvent::KeySequence(b"\x1b[15~".to_vec())),
            b"\x1b[15~",
            "mapped bytes are sent verbatim regardless of keyboard mode"
        );
        let empty = InputEvent::KeySequence(Vec::new());
        assert_eq!(egress_bytes(&term, &empty), b"");
        assert_eq!(egress_of(&term, &empty), Egress::Reported(Delivery::Full));
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

    #[test]
    fn key_and_text_egress_complete_while_the_terminal_mutex_is_held() {
        use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
        let term = term_with(&[b"\x1b[>1u"]);
        let modes = crate::mode_mirror_of(&term);
        let mut cap = CaptureSink::new();
        let sink = SinkWriter::new(cap.master());
        let held = term_lock(&term);
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
        let (a, b) = rx
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("the seam parked behind the terminal mutex to read keyboard_mode()");
        drop(held);
        worker.join().unwrap();
        assert_eq!(a, Egress::Reported(Delivery::Full));
        assert_eq!(b, Egress::Reported(Delivery::Full));
        assert_eq!(cap.drain(), b"\x1b[13;2u\xc3\xa9".to_vec());
    }

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

    #[cfg(unix)]
    fn detent_arrows(arrow: &[u8], notches: i32) -> Vec<u8> {
        let lines = wheel_platform_lines(WheelPlatform::Host, notches, 24, true);
        arrow.repeat(usize::try_from(lines).expect("a non-negative count"))
    }

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

    #[test]
    fn the_option_wheel_override_routes_locally_on_every_platform() {
        use aterm_types::mouse::{ALT_MASK, SHIFT_MASK};

        let route = |term: &Mutex<Terminal>, mods: u8| {
            let t = term.lock().expect("terminal");
            super::wheel_route_for(&t, mods)
        };

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

        let plain = term_with(&[]);
        assert_eq!(route(&plain, 0), WheelRoute::Viewport);
        assert_eq!(route(&plain, ALT_MASK), WheelRoute::Viewport);

        let main_1007 = term_with(&[b"\x1b[?1007h"]);
        assert_eq!(route(&main_1007, 0), WheelRoute::Viewport);
        let alt_1007_off = term_with(&[b"\x1b[?1049h", b"\x1b[?1007l"]);
        assert_eq!(route(&alt_1007_off, 0), WheelRoute::Viewport);
    }

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
        term.lock().unwrap().process(b"\x1b[?1h");
        assert_eq!(
            egress_bytes(&term, &wheel(WheelDir::Up, 1)),
            detent_arrows(b"\x1bOA", 1)
        );
        assert!(matches!(
            egress_of(&term, &wheel(WheelDir::Up, 1)),
            Egress::Reported(_)
        ));
    }

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
        assert_eq!(
            egress_bytes(&term, &wheel(WheelDir::Up, 1)),
            b"\x1b[<64;1;1M"
        );
    }

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

    #[test]
    fn the_report_burst_is_clamped_at_both_ends_for_every_source() {
        let term = term_with(&[b"\x1b[?1000h", b"\x1b[?1006h"]);
        let one = b"\x1b[<64;1;1M";
        assert_eq!(
            egress_bytes(&term, &wheel(WheelDir::Up, 3)).len(),
            one.len() * 3
        );
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
        assert_eq!(egress_bytes(&term, &wheel(WheelDir::Up, 0)), one);
        assert_eq!(egress_bytes(&term, &wheel(WheelDir::Up, -7)), one);
    }

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
        assert_eq!(wheel_scaled_lines(1, WheelNotch::Lines(3), 24), 3);
        assert_eq!(wheel_scaled_lines(4, WheelNotch::Lines(3), 24), 12);
        assert_eq!(wheel_scaled_lines(2, WheelNotch::Lines(1), 24), 2);
        assert_eq!(wheel_scaled_lines(1, WheelNotch::Page, 24), 24);
        assert_eq!(wheel_scaled_lines(2, WheelNotch::Page, 50), 100);
        assert_eq!(wheel_scaled_lines(1, WheelNotch::Page, 0), 1);
        assert_eq!(wheel_scaled_lines(9, WheelNotch::Lines(0), 24), 0);
        assert_eq!(
            wheel_scaled_lines(i32::MAX, WheelNotch::Lines(u32::MAX), 24),
            i32::MAX
        );
    }

    #[test]
    fn a_free_desktop_wheel_detent_is_vtes_distance() {
        let vte = WheelPlatform::Vte;
        assert_eq!(vte_lines_per_detent(24), 3);
        assert_eq!(wheel_platform_lines(vte, 1, 24, true), 3);
        assert_eq!(wheel_platform_lines(vte, 4, 24, true), 12);
        assert_eq!(vte_lines_per_detent(20), 2);
        assert_eq!(vte_lines_per_detent(21), 3);
        assert_eq!(vte_lines_per_detent(50), 5);
        assert_eq!(vte_lines_per_detent(10), 1);
        assert_eq!(vte_lines_per_detent(1), 1);
        assert_eq!(vte_lines_per_detent(0), 1);
        assert_eq!(vte_lines_per_detent(u16::MAX), 6554);
        assert_eq!(wheel_platform_lines(vte, i32::MAX, 24, true), i32::MAX);
        assert_eq!(wheel_platform_lines(WheelPlatform::Mac, 4, 24, true), 4);
    }

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

    #[test]
    fn builders_converge() {
        use aterm_types::keyboard::{Key, Modifiers, NamedKey};
        use aterm_window::keyboard::{
            Key as WinitKey, KeyCode, NamedKey as WinitNamed, PhysicalKey,
        };
        use aterm_winit_keymap::{base_layout_key_for, map_logical_key};

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
        for seqs in [&[][..], &[&b"\x1b[>1u"[..]][..]] {
            let term = term_with(seqs);
            assert_eq!(
                egress_bytes(&term, &human_ctrl_c),
                egress_bytes(&term, &ctrl_ctrl_c),
                "Ctrl+C bytes diverge (human base_layout vs controller None) under {seqs:?}",
            );
        }

        let ctrl_press =
            crate::control::parse_mouse("press left 5 9 mods=shift count=2 side=right block=1")
                .expect("mouse press parses");
        let human_press = InputEvent::MouseButton {
            button: aterm_types::mouse::MouseButton::Left,
            pressed: true,
            row: 5,
            col: 9,
            mods: aterm_types::mouse::SHIFT_MASK,
            click_count: 2,
            side: SelectionSide::Right,
            block: true,
            suppress_copy_on_select: false,
            px_off: PixelOffset::CELL_ORIGIN,
        };
        assert_eq!(human_press, ctrl_press, "mouse-press builder mismatch");

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

    #[test]
    fn sgr_pixel_1016_reports_true_pixel_coordinate() {
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
            px_off: PixelOffset { x: 4, y: 7 },
        };
        assert_eq!(egress_bytes(&term, &press), b"\x1b[<0;35;48M");

        let term_cell = term_with(&[b"\x1b[?1000h", b"\x1b[?1006h"]);
        assert_eq!(egress_bytes(&term_cell, &press), b"\x1b[<0;4;3M");
    }

    #[test]
    fn jump_prompt_target_finds_prompts_from_an_a_b_only_shell() {
        let mut t = Terminal::new(4, 20);
        t.process(b"\x1b]133;A\x1b\\p1>\x1b]133;B\x1b\\\r\n");
        for _ in 0..6 {
            t.process(b"out\r\n");
        }
        t.process(b"\x1b]133;A\x1b\\p2>\x1b]133;B\x1b\\\r\n");
        for _ in 0..6 {
            t.process(b"out\r\n");
        }
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

        t.scroll_to_top();
        let top = t.grid().top_visible_absolute_row();
        assert_eq!(
            jump_prompt_target(&t, false),
            prompts.iter().copied().filter(|&r| r > top).min(),
            "next-prompt must reach the nearest prompt below the viewport"
        );
    }

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
        assert_eq!(
            jump_prompt_target(&t, true),
            marks.iter().copied().filter(|&r| r < top).max()
        );
        assert_eq!(
            jump_prompt_target(&t, false),
            marks.iter().copied().filter(|&r| r > top).min()
        );
    }

    #[test]
    fn report_coords_selects_pixel_only_for_1016() {
        let mut t = Terminal::new(24, 80);
        t.set_cell_pixel_size(10, 20);
        t.process(b"\x1b[?1000h");
        t.process(b"\x1b[?1006h");
        assert_eq!(report_coords(&t, 3, 2, PixelOffset { x: 9, y: 19 }), (3, 2));
        t.process(b"\x1b[?1016h");
        assert_eq!(
            report_coords(&t, 3, 2, PixelOffset { x: 4, y: 7 }),
            (34, 47)
        );
        assert_eq!(
            report_coords(&t, 3, 2, PixelOffset { x: 99, y: 99 }),
            (3 * 10 + 9, 2 * 20 + 19)
        );
    }

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

    #[test]
    fn shell_escape_path_matches_iterm_set_exactly() {
        let set = "\\ ()\"&'!$<>;|*?[]#`\t{}^+=@~\r\n";
        let escaped = shell_escape_path_posix(set);
        let expected: String = set.chars().flat_map(|c| ['\\', c]).collect();
        assert_eq!(escaped, expected);
        let inert = "/.-_,:%0123456789abzABZ";
        assert_eq!(shell_escape_path_posix(inert), inert);
    }

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

    #[test]
    fn the_seam_frames_a_paste_by_the_event_not_by_the_moment() {
        let live_off = term_with(&[]);
        let live_on = term_with(&[b"\x1b[?2004h"]);
        let paste = |framing| InputEvent::Paste("ls\nrm -rf ~".to_string(), framing);

        assert_eq!(
            egress_bytes(&live_off, &paste(PasteFraming::AtDrain)),
            b"ls\rrm -rf ~".to_vec()
        );
        assert_eq!(
            egress_bytes(&live_on, &paste(PasteFraming::AtDrain)),
            b"\x1b[200~ls\rrm -rf ~\x1b[201~".to_vec()
        );

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

    #[cfg(unix)]
    #[test]
    fn dropped_path_pastes_escaped_with_trailing_space() {
        let drop_text = |p: &str| {
            let mut t = shell_escape_path(p);
            t.push(' ');
            t
        };
        let term = term_with(&[]);
        assert_eq!(
            egress_bytes(
                &term,
                &InputEvent::Paste(drop_text("/p/My File"), PasteFraming::AtDrain)
            ),
            b"/p/My\\ File ".to_vec()
        );
        let term = term_with(&[b"\x1b[?2004h"]);
        assert_eq!(
            egress_bytes(
                &term,
                &InputEvent::Paste(drop_text("/p/My File"), PasteFraming::AtDrain)
            ),
            b"\x1b[200~/p/My\\ File \x1b[201~".to_vec()
        );
    }

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

    #[test]
    fn shell_escape_path_windows_quotes_metachars_and_doubles_quotes() {
        assert_eq!(shell_escape_path_windows("C:\\a,b.txt"), "\"C:\\a,b.txt\"");
        assert_eq!(
            shell_escape_path_windows("C:\\a&b;c|d.txt"),
            "\"C:\\a&b;c|d.txt\""
        );
        assert_eq!(shell_escape_path_windows("a\"b"), "\"a\"\"b\"");
    }

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
            ("O'Neil $(x).pdf", "'O''Neil $(x).pdf'"),
            (
                "a\u{2019};calc;\u{2018}$(x).pdf",
                "'a\u{2019}\u{2019};calc;\u{2018}\u{2018}$(x).pdf'",
            ),
            ("a&calc&$(x).pdf", "'a^&calc^&$(x).pdf'"),
            ("a^&$(x).pdf", "'a^^^&$(x).pdf'"),
        ] {
            assert_eq!(
                shell_escape_path_windows(&format!("{dl}{name}")),
                want.replacen('\'', &format!("'{dl}"), 1),
                "{name:?}"
            );
        }
        assert_eq!(
            shell_escape_path_windows("\\\\host\\C$\\My Docs\\a.txt"),
            "\"\\\\host\\C$\\My Docs\\a.txt\""
        );
        assert_eq!(shell_escape_path_windows("C:\\x y$"), "\"C:\\x y$\"");
    }

    #[test]
    fn paths_paste_insertion_matches_the_drop_contract() {
        let one = vec!["C:\\Users\\me\\a.txt".to_string()];
        assert_eq!(
            paths_paste_insertion(&one),
            format!("{} ", shell_escape_path("C:\\Users\\me\\a.txt"))
        );
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
        assert_eq!(paths_paste_insertion(&[]), "");
        assert_eq!(paths_paste_insertion(&[String::new()]), "");
    }

    #[test]
    fn wheel_route_shift_bypasses_tracking_before_every_other_test() {
        use super::{WheelRoute, wheel_route};
        for tracking in [false, true] {
            for alt_scroll in [false, true] {
                assert_eq!(
                    wheel_route(true, false, tracking, alt_scroll),
                    WheelRoute::Viewport,
                    "shift must bypass (tracking={tracking}, alt_scroll={alt_scroll})"
                );
            }
        }
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
        assert_eq!(wheel_route(false, true, true, false), WheelRoute::Viewport);
        assert_eq!(wheel_route(false, true, false, false), WheelRoute::Viewport);
        assert_eq!(wheel_route(false, true, true, true), WheelRoute::Viewport);
        assert_eq!(wheel_route(true, true, true, true), WheelRoute::Viewport);
    }
}
