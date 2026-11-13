// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Engine-neutral [`InputEvent`] vocabulary and [`Source`] audit tag for the input convergence seam.

use std::sync::Mutex;
use std::sync::atomic::AtomicU32;

use aterm_core::selection::SelectionSide;
use aterm_core::terminal::{ModeMirror, Terminal};
use aterm_session::sink::{AcceptedOrder, BulkMeter, SinkWriter, WriteReceipt, WriteReceiptError};
use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey, encode_key_with_event};
use aterm_types::mouse::MouseButton;

use crate::metrics::TermWaitSite;
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
    /// A geometry request found no window to change, so nothing was applied; the words say why.
    NotApplied(&'static str),
    /// A window resize (either form of the `resize` verb) was applied: what was asked and what
    /// the window was given, which differ where the least-size rule held an axis
    /// (`App::held_window_size`).
    Resized(WindowResize),
}

/// What one window resize asked for and what the window was given ([`InputOutcome::Resized`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowResize {
    /// The grid form: `(rows, cols)` asked, and the grid the window took.
    Grid {
        asked: (u16, u16),
        given: (u16, u16),
    },
    /// The pixel form: `(width, height)` device pixels asked, and the size the window was asked
    /// for after the least-size rule (a window manager may still change a platform window's).
    Px {
        asked: (u32, u32),
        given: (u32, u32),
    },
}

impl WindowResize {
    /// Whether the least-size rule gave the window more than was asked.
    pub(crate) const fn held(self) -> bool {
        match self {
            Self::Grid { asked, given } => asked.0 != given.0 || asked.1 != given.1,
            Self::Px { asked, given } => asked.0 != given.0 || asked.1 != given.1,
        }
    }

    /// The `resize` verb's `OK` line: the size given, then, where the least-size rule held it,
    /// `held=least` and the size asked.
    pub(crate) fn reply(self) -> String {
        let held = self.held();
        match self {
            Self::Grid { asked, given } if held => format!(
                "OK rows={} cols={} held=least asked_rows={} asked_cols={}\n",
                given.0, given.1, asked.0, asked.1
            ),
            Self::Grid { given, .. } => format!("OK rows={} cols={}\n", given.0, given.1),
            Self::Px { asked, given } if held => format!(
                "OK width={} height={} held=least asked_width={} asked_height={}\n",
                given.0, given.1, asked.0, asked.1
            ),
            Self::Px { given, .. } => format!("OK width={} height={}\n", given.0, given.1),
        }
    }
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
    drain: Option<&SinkWriter>,
) -> (Delivery, Option<AcceptedOrder>, bool, bool) {
    let drained = drain.is_none_or(|sink| {
        let (order, prefix) = match &result {
            Ok(receipt) => (receipt.order(), receipt.kernel_prefix()),
            Err(error) => (error.order(), error.kernel_prefix()),
        };
        order.is_none() || prefix.is_some_and(|prefix| sink.wait_prefix_to_kernel(prefix))
    });
    match result {
        Ok(receipt) => (
            if drained {
                delivered(Ok(receipt.accepted()), intended)
            } else {
                Delivery::Failed
            },
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
    /// An expendable writer that owns and drains exactly each admitted frame's kernel prefix.
    PrefixDrained,
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
                None,
            )
        }
        EgressMode::Backpressured => {
            crate::echo_rtt::note_write_start();
            delivered_receipt(sink.write_frame_with_receipt(bytes), bytes.len(), None)
        }
        EgressMode::PrefixDrained => {
            crate::echo_rtt::note_write_start();
            delivered_receipt(sink.write_frame_with_prefix(bytes), bytes.len(), Some(sink))
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
    mode: EgressMode,
) -> Delivery {
    crate::echo_rtt::note_write_start();
    let (delivery, order, direct, _) = delivered_receipt(
        if mode == EgressMode::PrefixDrained {
            sink.write_frame_metered_with_prefix(bytes, meter)
        } else {
            sink.write_frame_metered_with_receipt(bytes, meter)
        },
        bytes.len(),
        (mode == EgressMode::PrefixDrained).then_some(sink),
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
        None,
    )
}

/// The source-blind byte-producing core of the seam (design A.2.1).
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
    seam_egress_receipt_as(term, modes, sink, ev, mode, meter, None)
}

/// [`seam_egress_receipt`] for a pointer report on the UI thread: its terminal wait registers in
/// `ui_waiting`, so the PTY reader hands the lock over, and books under [`TermWaitSite::Mouse`].
pub(crate) fn seam_egress_receipt_ui(
    term: &Mutex<Terminal>,
    modes: &ModeMirror,
    sink: &SinkWriter,
    ev: &InputEvent,
    mode: EgressMode,
    ui_waiting: &AtomicU32,
) -> EgressReceipt {
    seam_egress_receipt_as(term, modes, sink, ev, mode, None, Some(ui_waiting))
}

fn seam_egress_receipt_as(
    term: &Mutex<Terminal>,
    modes: &ModeMirror,
    sink: &SinkWriter,
    ev: &InputEvent,
    mode: EgressMode,
    meter: Option<&BulkMeter>,
    ui_waiting: Option<&AtomicU32>,
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
        ui_waiting,
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

/// [`seam_encode`] for a pointer report on the UI thread ([`seam_egress_receipt_ui`]).
pub(crate) fn seam_encode_ui(
    term: &Mutex<Terminal>,
    modes: &ModeMirror,
    sink: &SinkWriter,
    ev: &InputEvent,
    ui_waiting: &AtomicU32,
) -> (Egress, Vec<u8>) {
    seam_encode_as(WheelPlatform::Host, term, modes, sink, ev, Some(ui_waiting))
}

/// [`seam_encode`] on the wheel rule `platform` ([`WheelPlatform`]).
pub(crate) fn seam_encode_on(
    platform: WheelPlatform,
    term: &Mutex<Terminal>,
    modes: &ModeMirror,
    sink: &SinkWriter,
    ev: &InputEvent,
) -> (Egress, Vec<u8>) {
    seam_encode_as(platform, term, modes, sink, ev, None)
}

fn seam_encode_as(
    platform: WheelPlatform,
    term: &Mutex<Terminal>,
    modes: &ModeMirror,
    sink: &SinkWriter,
    ev: &InputEvent,
    ui_waiting: Option<&AtomicU32>,
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
        ui_waiting,
    );
    (egress, accepted.encoded)
}

/// The bytes the seam writes for a key chord or committed text under `keyboard`, the program's
/// keyboard mode: a key as the key encoder writes it, committed text as the IME commit path
/// writes it; `None` for any other event. A typed turn's rest writes the same bytes for a run of
/// its keys in one write (`control_session::rest_run_event`), so typing a text and the
/// runs of its rest are one encoding.
pub(crate) fn typed_bytes(
    ev: &InputEvent,
    keyboard: aterm_types::keyboard::KeyboardMode,
) -> Option<Vec<u8>> {
    match ev {
        InputEvent::Key {
            key,
            mods,
            base_layout,
            layout_text,
            event_type,
        } => Some(aterm_types::keyboard::encode_key_with_text(
            key,
            *mods,
            keyboard,
            *event_type,
            *base_layout,
            *layout_text,
        )),
        InputEvent::Text(text) if text.is_empty() => Some(Vec::new()),
        InputEvent::Text(text) => Some(crate::keymap::encode_committed_text(text, keyboard)),
        _ => None,
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the seam's six handles, the paste's meter, the injected wheel rule and the UI \
              thread's wait count; bundling them would add a per-event struct on the keystroke path"
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
    ui_waiting: Option<&AtomicU32>,
) -> Egress {
    #[cfg(debug_assertions)]
    if let Ok(t) = term.try_lock() {
        debug_assert!(
            std::ptr::eq(modes, &**t.mode_mirror()),
            "seam handed a ModeMirror that is not this terminal's (see mode_mirror_of)"
        );
    }
    match ev {
        InputEvent::Key { .. } | InputEvent::Text(_) => {
            let bytes = typed_bytes(ev, modes.keyboard_mode()).unwrap_or_default();
            let d = if bytes.is_empty() {
                Delivery::Full
            } else {
                emit(sink, mode, &bytes, accepted_order)
            };
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
                let mut t = match ui_waiting {
                    Some(waiting) => crate::term_lock_ui(term, waiting, TermWaitSite::Mouse),
                    None => term_lock(term),
                };
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
                let mut t = match ui_waiting {
                    Some(waiting) => crate::term_lock_ui(term, waiting, TermWaitSite::Mouse),
                    None => term_lock(term),
                };
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
                let mut t = match ui_waiting {
                    Some(waiting) => crate::term_lock_ui(term, waiting, TermWaitSite::Mouse),
                    None => term_lock(term),
                };
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
                Some(meter)
                    if !out.is_empty()
                        && matches!(
                            mode,
                            EgressMode::Backpressured | EgressMode::PrefixDrained
                        ) =>
                {
                    emit_metered(sink, &out, accepted_order, meter, mode)
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
#[path = "input_tests.rs"]
mod tests;
