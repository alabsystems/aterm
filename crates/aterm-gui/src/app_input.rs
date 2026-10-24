// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keyboard, IME, mouse-modifier, and action dispatch methods on `App`.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use aterm_core::selection::SelectionType;
use aterm_core::terminal::{CustodyTransition, ModeMirror, Terminal};
use aterm_effects::cursor_glow::{DeliveredClass, InsertWidth};
use aterm_effects::kitty_pet::PetInputKind;
use aterm_effects::typed_tricks::{TrickEvent, TrickFeed};
use aterm_session::sink::{AcceptedOrder, SinkWriter};
use aterm_window::event::{ElementState, KeyEvent};
use aterm_window::event_loop::ActiveEventLoop;
use aterm_window::keyboard::{Key, ModifiersState, NamedKey};

use crate::input::{self, InputEvent, InputOutcome, ScrollIntent, Source};
use crate::{App, FONT_ZOOM_STEP, Wake, WindowId, keybinding, keymap, menu, pane, term_lock};

#[cfg(clean_islands)]
macro_rules! trust_island {
    ($($item:item)*) => {
        $($item)*
    };
}

#[cfg(not(clean_islands))]
macro_rules! trust_island {
    ($($tt:tt)*) => {};
}

trust_island! {
    clean {
        namespace InputPressLockDiscipline

        -- Model of OutputEchoWrite::finish publishing (key_at, sent_at) in one
        -- atomic hold vs two separate holds (publish then publish_delivery).
        structure EchoSlot where
          seq : Nat
          hasKeyAt : Bool
          hasSentAt : Bool

        def publishCoalesced (seq : Nat) (drained : Bool) : EchoSlot :=
          EchoSlot.mk seq true drained

        def publishSplitStep1 (seq : Nat) : EchoSlot :=
          EchoSlot.mk seq true false

        def publishSplitStep2 (slot : EchoSlot) (drained : Bool) : EchoSlot :=
          EchoSlot.mk slot.seq slot.hasKeyAt drained

        -- Coalesced publish never exposes an intermediate state where hasKeyAt is
        -- true but hasSentAt is false for an already-drained direct write.
        theorem coalesced_direct_write_immediately_has_sent_at (seq : Nat) :
            (publishCoalesced seq true).hasSentAt = true := rfl

        -- Negative control: split publish exposes hasSentAt = false between Hold 1
        -- and Hold 2 even when the write drained directly to the kernel.
        theorem split_direct_write_exposes_missing_sent_at_between_holds (seq : Nat) :
            (publishSplitStep1 seq).hasSentAt = false := rfl

        -- Model of PTY reader batch cap selection (8 KiB interactive vs 64 KiB bulk)
        -- when input_hint::arm (inputPending) runs before vs after term_lock_ui.
        def ptyBatchCap (inputPending : Bool) : Nat :=
          if inputPending = true then 8192 else 65536

        theorem pre_lock_input_pending_caps_pty_slice_at_8k :
            ptyBatchCap true = 8192 := rfl

        theorem post_lock_unmarked_input_leaves_pty_slice_at_64k :
            ptyBatchCap false = 65536 := rfl

        end InputPressLockDiscipline
    }
}

/// The input path's one clock read.
fn input_clock() -> std::time::Instant {
    #[cfg(test)]
    if let Some(now) = INPUT_CLOCK.with(std::cell::Cell::get) {
        return now;
    }
    std::time::Instant::now()
}

#[cfg(test)]
thread_local! {
    static INPUT_CLOCK: std::cell::Cell<Option<std::time::Instant>> =
        const { std::cell::Cell::new(None) };
}

/// Pin (or, with `None`, release) this thread's input clock ([`input_clock`]).
#[cfg(test)]
pub(crate) fn pin_input_clock(now: Option<std::time::Instant>) {
    INPUT_CLOCK.with(|clock| clock.set(now));
}

/// Where a predict test's timeline starts.
#[cfg(test)]
pub(crate) fn past_timeline_start() -> std::time::Instant {
    const BACK_MS: u64 = 10_000;
    const _: () = assert!(
        BACK_MS > aterm_predict::compute_glitch_window_ms(Some(u32::MAX), u8::MAX),
        "the timeline starts further back than any glitch window"
    );
    std::time::Instant::now()
        .checked_sub(std::time::Duration::from_millis(BACK_MS))
        .expect("the monotonic clock is more than ten seconds old")
}

/// How long after a keystroke the present-pacing bypass (`WindowState::input_hot`) stays armed.
pub(crate) const INPUT_HOT_WINDOW: std::time::Duration = std::time::Duration::from_millis(50);

const ORDERED_EGRESS_WITNESS_GRACE: std::time::Duration = std::time::Duration::from_millis(1_500);

/// Per-session evidence that recently accepted input can explain output.
#[derive(Default)]
pub(crate) struct OutputEchoTracker {
    active_writes: AtomicUsize,
    /// Every attempt to write input to the session.
    sent: AtomicU64,
    published: Mutex<OutputEchoPublished>,
    /// The Enters among those writes that submitted something composed since the last
    /// ([`crate::submit_count`], [`Self::submits`]).
    submit: crate::submit_count::SubmitCount,
}

/// How a paste's insert is priced for the cursor engine ([`DeliveryTicket::insert`]).
pub(crate) fn paste_insert_width(text: &str, ambiguous_width_double: bool) -> InsertWidth {
    use aterm_core::terminal::Terminal;
    if let Some(cells) = Terminal::paste_insert_cells(text, ambiguous_width_double) {
        return InsertWidth::Cells(cells);
    }
    text.strip_suffix("\r\n")
        .or_else(|| text.strip_suffix(['\n', '\r']))
        .and_then(|line| Terminal::paste_insert_cells(line, ambiguous_width_double))
        .filter(|&cells| cells > 0)
        .map_or(InsertWidth::Unknown, InsertWidth::Line)
}

/// What a queued write's completion re-stamps on the cursor engines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DeliveryTicket {
    /// A delivered insert's width.
    pub(crate) insert: Option<InsertWidth>,
    /// The window that dispatched a key.
    pub(crate) window: Option<WindowId>,
    /// The key's licence class, re-stamped at delivery on [`Self::window`].
    pub(crate) key: Option<DeliveredClass>,
    /// When the bytes were dispatched at the input seam.
    pub(crate) dispatched_at: std::time::Instant,
}

impl DeliveryTicket {
    const fn is_empty(self) -> bool {
        self.insert.is_none() && self.key.is_none()
    }
}

/// One completed delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DeliveryReceipt {
    pub(crate) serial: u64,
    delivered_us: u64,
    pub(crate) ticket: DeliveryTicket,
    /// The write failed.
    failed: bool,
}

/// How many delivery receipts the tracker keeps for the render prelude.
pub(crate) const DELIVERY_RING: usize = 2 * aterm_effects::cursor_glow::TYPED_STAMP_DEPTH;

/// The deliveries a window has not yet applied ([`OutputEchoTracker::deliveries_after`]).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Deliveries {
    /// The newest resolved serial.
    pub(crate) latest: u64,
    /// `(delivery instant on the frame's clock, ticket)`, oldest first.
    pub(crate) items: [Option<(std::time::Instant, DeliveryTicket)>; DELIVERY_RING],
    /// The queued writes that failed, oldest first.
    pub(crate) failed: [Option<DeliveryTicket>; DELIVERY_RING],
    /// Receipts newer than the reader's baseline that the ring had overwritten before this read.
    pub(crate) evicted: u64,
}

impl Deliveries {
    /// Nothing to apply, read up to `latest`.
    const fn none_after(latest: u64) -> Self {
        Self {
            latest,
            items: [None; DELIVERY_RING],
            failed: [None; DELIVERY_RING],
            evicted: 0,
        }
    }
}

/// Acceptance-ordered echo evidence.
struct OutputEchoPublished {
    order: Option<AcceptedOrder>,
    last_accepted_us: u64,
    last_boundary_us: u64,
    spill_debt: Option<bool>,
    deliveries: [Option<DeliveryReceipt>; DELIVERY_RING],
    delivery_head: usize,
    delivery_serial: u64,
}

impl Default for OutputEchoPublished {
    fn default() -> Self {
        Self {
            order: None,
            last_accepted_us: 0,
            last_boundary_us: 0,
            spill_debt: None,
            deliveries: [None; DELIVERY_RING],
            delivery_head: 0,
            delivery_serial: 0,
        }
    }
}

impl OutputEchoPublished {
    /// Write `receipt` at the ring's head, the oldest slot.
    const fn push_receipt(&mut self, receipt: DeliveryReceipt) {
        let head = self.delivery_head;
        self.deliveries[head] = Some(receipt);
        self.delivery_head = (head + 1) % DELIVERY_RING;
    }

    /// A receipt held in the spill settles at the first instant the spill is seen empty.
    fn settle_held_receipts(&mut self, stamp_us: u64) {
        for r in self.deliveries.iter_mut().flatten() {
            if r.delivered_us == 0 {
                r.delivered_us = stamp_us;
            }
        }
    }
}

/// A metrics-clock stamp (`crate::metrics::now_us`) placed on the frame's clock.
fn stamp_on_frame_clock(stamp_us: u64, now: std::time::Instant) -> std::time::Instant {
    crate::metrics::instant_at_us(stamp_us).map_or(now, |at| at.min(now))
}

/// One coherent render-time view of a session's echo evidence.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct OutputEchoSample {
    pub(crate) input_hot: bool,
    pub(crate) last_accepted_at: Option<std::time::Instant>,
    pub(crate) last_boundary_at: Option<std::time::Instant>,
}

impl OutputEchoSample {
    /// The facts prism wake's licence reads.
    pub(crate) const fn streak(self) -> aterm_effects::pipeline::StreakEcho {
        aterm_effects::pipeline::StreakEcho {
            boundary_at: self.last_boundary_at,
            accepted_at: self.last_accepted_at,
            input_hot: self.input_hot,
        }
    }
}

/// Raii ownership of one real egress attempt.
pub(crate) struct OutputEchoWrite<'a> {
    tracker: &'a OutputEchoTracker,
    counted: bool,
    kind: OutputEchoInput,
    /// The queued write's [`DeliveryTicket`], published on acceptance.
    ticket: Option<DeliveryTicket>,
    /// What the write means for the session's submission count
    /// ([`crate::submit_count::SubmitInput`]).
    submit: crate::submit_count::SubmitInput,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutputEchoInput {
    Echoable,
    TurnBoundary,
    Ignored,
}

impl OutputEchoInput {
    fn raw_editor(bytes: &[u8]) -> Self {
        if bytes
            .last()
            .is_some_and(|byte| matches!(byte, b'\r' | b'\n'))
        {
            Self::TurnBoundary
        } else {
            Self::Echoable
        }
    }

    fn of(ev: &InputEvent) -> Self {
        let release = matches!(
            ev,
            InputEvent::Key {
                event_type: aterm_types::keyboard::KeyEventType::Release,
                ..
            }
        );
        match ev {
            InputEvent::Key { .. } if is_plain_enter(ev) && !release => Self::TurnBoundary,
            InputEvent::KeySequence(bytes) => Self::raw_editor(bytes),
            InputEvent::Key { .. } | InputEvent::Text(_) | InputEvent::Paste(..) => Self::Echoable,
            _ => Self::Ignored,
        }
    }
}

impl OutputEchoTracker {
    /// Begin immediately before the real PTY seam (never at FIFO admission).
    fn begin(&self, kind: OutputEchoInput) -> OutputEchoWrite<'_> {
        let counted = kind != OutputEchoInput::Ignored
            && self
                .active_writes
                .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                    active.checked_add(1)
                })
                .is_ok();
        if kind != OutputEchoInput::Ignored {
            self.sent.fetch_add(1, Ordering::AcqRel);
        }
        OutputEchoWrite {
            tracker: self,
            counted,
            kind,
            ticket: None,
            submit: crate::submit_count::SubmitInput::Neither,
        }
    }

    /// [`Self::begin`] for a queued write carrying a [`DeliveryTicket`].
    fn begin_ticketed(
        &self,
        kind: OutputEchoInput,
        ticket: Option<DeliveryTicket>,
    ) -> OutputEchoWrite<'_> {
        let mut write = self.begin(kind);
        write.ticket = ticket.filter(|t| !t.is_empty());
        write
    }

    /// How many input writes have been attempted on the session.
    #[must_use]
    pub(crate) fn input_sent(&self) -> u64 {
        self.sent.load(Ordering::Acquire)
    }

    /// [`Self::input_sent`] where no tracked write is in flight and the actual sink has neither a
    /// partial frame nor process-local spill.
    #[must_use]
    pub(crate) fn input_settled(&self, sink: &SinkWriter) -> Option<u64> {
        let sent = self.sent.load(Ordering::Acquire);
        if self.active_writes.load(Ordering::Acquire) != 0 {
            return None;
        }
        let _fence = sink.try_fence_egress()?;
        Some(sent)
    }

    /// How many Enters written to the session submitted something typed since the one before
    /// ([`crate::submit_count`]).
    #[must_use]
    pub(crate) fn submits(&self) -> u64 {
        self.submit.submits()
    }

    pub(crate) fn begin_event(&self, ev: &InputEvent) -> OutputEchoWrite<'_> {
        let mut write = self.begin(OutputEchoInput::of(ev));
        write.submit = crate::submit_count::SubmitInput::of(ev);
        write
    }

    /// The render prelude's read.
    pub(crate) fn deliveries_after(
        &self,
        sink: &SinkWriter,
        after: Option<u64>,
        now: std::time::Instant,
    ) -> Deliveries {
        let clock_us = crate::metrics::now_us().max(1);
        let mut published = self
            .published
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(after) = after else {
            return Deliveries::none_after(published.delivery_serial);
        };
        let mut out = Deliveries::none_after(after);
        if published.delivery_serial <= after {
            return out;
        }
        let drained = published
            .deliveries
            .iter()
            .flatten()
            .any(|r| r.delivered_us == 0)
            && sink.try_egress_drained_to_kernel() == Some(true);
        if drained {
            published.settle_held_receipts(clock_us);
        }
        let head = published.delivery_head;
        let mut slots = out.items.iter_mut();
        let mut failed = out.failed.iter_mut();
        for r in published
            .deliveries
            .iter()
            .cycle()
            .skip(head)
            .take(DELIVERY_RING)
            .flatten()
        {
            if r.serial <= after {
                continue;
            }
            if r.delivered_us == 0 {
                break;
            }
            if r.failed {
                let Some(slot) = failed.next() else {
                    break;
                };
                *slot = Some(r.ticket);
                out.latest = r.serial;
                continue;
            }
            let Some(slot) = slots.next() else {
                break;
            };
            let at = stamp_on_frame_clock(r.delivered_us, now).max(r.ticket.dispatched_at);
            *slot = Some((at, r.ticket));
            out.latest = r.serial;
        }
        out.evicted = if out.latest > after {
            published
                .delivery_serial
                .saturating_sub(DELIVERY_RING as u64)
                .saturating_sub(after)
        } else {
            0
        };
        out
    }

    /// Samples echo evidence against the render frame's injected clock.
    pub(crate) fn sample(&self, sink: &SinkWriter, now: std::time::Instant) -> OutputEchoSample {
        let clock_us = crate::metrics::now_us().max(1);
        let mut published = match self.published.try_lock() {
            Ok(published) => published,
            Err(std::sync::TryLockError::Poisoned(poison)) => poison.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                return OutputEchoSample {
                    input_hot: true,
                    ..OutputEchoSample::default()
                };
            }
        };
        if published.spill_debt.is_some()
            && sink.try_egress_drained_to_kernel() == Some(true)
            && published.spill_debt.take() == Some(true)
        {
            published.last_accepted_us = clock_us;
        }
        let last_us = published.last_accepted_us;
        let boundary_us = published.last_boundary_us;
        let debt = published.spill_debt.is_some();
        let at = |stamp| stamp_on_frame_clock(stamp, now);
        let last_accepted_at = (last_us != 0).then(|| at(last_us));
        let last_boundary_at = (boundary_us != 0).then(|| at(boundary_us));
        OutputEchoSample {
            input_hot: self.active_writes.load(Ordering::Acquire) != 0 || debt,
            last_accepted_at,
            last_boundary_at,
        }
    }

    /// Publish one state-affecting acceptance in sink order.
    fn publish(
        &self,
        order: AcceptedOrder,
        kind: OutputEchoInput,
        stamp: u64,
        direct: bool,
        ticket: Option<DeliveryTicket>,
        sink: &SinkWriter,
    ) {
        let delivery_drained = ticket
            .is_some()
            .then(|| direct || sink.egress_drained_to_kernel());
        let mut published = self
            .published
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if !published.order.is_some_and(|current| current >= order) {
            published.order = Some(order);
            match kind {
                OutputEchoInput::Echoable => {
                    published.last_accepted_us = stamp;
                    published.spill_debt = Some(true);
                }
                OutputEchoInput::TurnBoundary => {
                    published.last_boundary_us = stamp;
                    published.last_accepted_us = 0;
                    published.spill_debt = Some(false);
                }
                OutputEchoInput::Ignored => unreachable!("ignored input has no echo publication"),
            }
            if published.spill_debt.is_some()
                && (direct
                    || delivery_drained == Some(true)
                    || sink.try_egress_drained_to_kernel() == Some(true))
                && published.spill_debt.take() == Some(true)
            {
                published.last_accepted_us = stamp;
            }
        }
        if let Some(ticket) = ticket {
            let drained = delivery_drained.unwrap_or(false);
            published.delivery_serial += 1;
            if drained {
                published.settle_held_receipts(stamp);
            }
            let receipt = DeliveryReceipt {
                serial: published.delivery_serial,
                delivered_us: if drained { stamp } else { 0 },
                ticket,
                failed: false,
            };
            published.push_receipt(receipt);
        }
    }

    /// Publish one ticketed write that failed.
    fn publish_failed(&self, ticket: DeliveryTicket, stamp: u64) {
        let mut published = self
            .published
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        published.delivery_serial += 1;
        let receipt = DeliveryReceipt {
            serial: published.delivery_serial,
            delivered_us: stamp,
            ticket,
            failed: true,
        };
        published.push_receipt(receipt);
    }
}

impl OutputEchoWrite<'_> {
    /// Complete with the seam's narrow receipt.
    fn finish(mut self, receipt: input::EgressReceipt, sink: &SinkWriter) {
        // THE SUBMISSION COUNT, at the write's full acceptance: before the
        // program can have read it, so a screen that shows its answer is read
        // before a count that holds it.
        if receipt.fully_accepted_nonempty() {
            self.tracker.submit.accepted(self.submit);
        }
        if self.kind != OutputEchoInput::Ignored
            && let Some(order) = receipt.accepted_order()
        {
            let stamp = crate::metrics::now_us().max(1);
            let ticket = self.ticket.take();
            self.tracker
                .publish(order, self.kind, stamp, receipt.is_direct(), ticket, sink);
        } else if receipt.egress == input::Egress::Reported(input::Delivery::Failed)
            && let Some(ticket) = self.ticket.take()
        {
            self.tracker
                .publish_failed(ticket, crate::metrics::now_us().max(1));
        }
        self.release();
    }

    pub(crate) fn finish_delivery(
        self,
        delivery: input::Delivery,
        accepted_order: Option<AcceptedOrder>,
        sink: &SinkWriter,
    ) {
        self.finish(
            input::EgressReceipt::from_reported_delivery(delivery, accepted_order),
            sink,
        );
    }

    fn release(&mut self) {
        if self.counted {
            self.tracker.active_writes.fetch_sub(1, Ordering::AcqRel);
            self.counted = false;
        }
    }
}

/// One receipt-bearing, echo-aware direct egress.
pub(crate) fn tracked_egress(
    term: &Mutex<Terminal>,
    modes: &ModeMirror,
    sink: &SinkWriter,
    tracker: &OutputEchoTracker,
    ev: &InputEvent,
    mode: input::EgressMode,
) -> input::EgressReceipt {
    tracked_egress_ticketed(term, modes, sink, tracker, ev, mode, None)
}

/// The one egress law, its ticket published as a [`DeliveryReceipt`] on acceptance.
pub(crate) fn tracked_egress_ticketed(
    term: &Mutex<Terminal>,
    modes: &ModeMirror,
    sink: &SinkWriter,
    tracker: &OutputEchoTracker,
    ev: &InputEvent,
    mode: input::EgressMode,
    ticket: Option<DeliveryTicket>,
) -> input::EgressReceipt {
    tracked_egress_metered(term, modes, sink, tracker, ev, mode, ticket, None)
}

#[allow(
    clippy::too_many_arguments,
    reason = "the one egress law's six handles plus the paste's meter; bundling them would \
              add a per-event struct on the keystroke path"
)]
pub(crate) fn tracked_egress_metered(
    term: &Mutex<Terminal>,
    modes: &ModeMirror,
    sink: &SinkWriter,
    tracker: &OutputEchoTracker,
    ev: &InputEvent,
    mode: input::EgressMode,
    ticket: Option<DeliveryTicket>,
    meter: Option<&aterm_session::sink::BulkMeter>,
) -> input::EgressReceipt {
    let mut write = tracker.begin_ticketed(OutputEchoInput::of(ev), ticket);
    write.submit = crate::submit_count::SubmitInput::of(ev);
    let receipt = input::seam_egress_receipt_metered(term, modes, sink, ev, mode, meter);
    write.finish(receipt, sink);
    receipt
}

impl Drop for OutputEchoWrite<'_> {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
#[path = "app_input_output_echo_tracker_tests.rs"]
mod output_echo_tracker_tests;

/// The IME composition clock's transition on one preedit event (`WindowState::ime_compose`).
fn ime_compose_on_preedit(
    open: Option<(std::time::Instant, bool)>,
    composing: bool,
    now: std::time::Instant,
) -> Option<(std::time::Instant, bool)> {
    match (open, composing) {
        (Some((started, false)), true) => Some((started, false)),
        (Some((_, true)), true) => {
            crate::metrics::note_ime_cancelled();
            Some((now, false))
        }
        (None, true) => Some((now, false)),
        (Some((started, _)), false) => Some((started, true)),
        (None, false) => None,
    }
}

/// The surface that owns one window's in-flight IME composition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PreeditOwner {
    /// No composition in flight.
    None,
    /// A frontmost native view (Settings, editor).
    Native,
    /// The inline tab-rename field.
    Rename,
    /// The open find field.
    Find,
    /// No field has focus.
    Grid,
}

#[inline]
const fn prediction_visibility_requires_redraw(was_visible: bool, is_visible: bool) -> bool {
    was_visible || is_visible
}

const _: () = assert!(
    prediction_visibility_requires_redraw(true, false)
        && prediction_visibility_requires_redraw(false, true)
        && !prediction_visibility_requires_redraw(false, false)
);

/// Everything the key-time typing click needs from App-level state to reach the synth without a
/// render tick.
#[derive(Clone, Copy)]
struct KeyClickSynth {
    style: crate::cursor_glow::GlowStyle,
    voice: aterm_effects::trail_sound::SoundVoice,
    gain: f32,
    bed: bool,
    tone_melody: bool,
}

/// Whether a typing click cued at the key could reach a speaker.
#[inline]
pub(crate) fn keystroke_click_audible(
    host: crate::trail_audio::HostState,
    sounds_on: bool,
    volume: f32,
    sound_allowed: bool,
    resize_quiet: bool,
) -> bool {
    crate::sound_seam::sound_seam(&crate::sound_seam::SeamInputs {
        sounds_on,
        volume,
        serious_allows: sound_allowed,
        resize_quiet,
        host: if matches!(host, crate::trail_audio::HostState::Inert) {
            crate::trail_audio::HostState::Inert
        } else {
            crate::trail_audio::HostState::Running
        },
        trail_on: true,
        focused: true,
        engine_sound_live: true,
    })
    .is_none()
}

/// What kind of glyph the key landed ([`aterm_effects::rainbow_kitty::TypedClass`]).
#[inline]
pub(crate) const fn typed_class_for(
    typed: Option<char>,
) -> aterm_effects::rainbow_kitty::TypedClass {
    use aterm_effects::rainbow_kitty::TypedClass;
    match typed {
        Some(' ') => TypedClass::Space,
        Some('!') => TypedClass::Bang,
        Some(c) if c.is_uppercase() => TypedClass::Capital,
        _ => TypedClass::Glyph,
    }
}

/// The synth's glyph class ([`aterm_effects::trail_sound::EventMeta::glyph_class`]).
#[inline]
pub(crate) const fn typed_glyph_class(typed: Option<char>) -> u8 {
    aterm_effects::trail_sound::typed_glyph_class(typed)
}

/// What the keyed click says about its key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct KeyClickStamp {
    pub(crate) kind: aterm_effects::trail_sound::SoundKind,
    pub(crate) shifted: bool,
    pub(crate) glyph_class: u8,
    pub(crate) rank: u8,
}

/// The keyed click's stamp.
pub(crate) fn key_click_stamp(
    secret: bool,
    spacebar: bool,
    shifted: bool,
    typed: Option<char>,
) -> KeyClickStamp {
    use aterm_effects::trail_sound::{SoundKind, glyph_class::SECRET};
    if secret {
        return KeyClickStamp {
            kind: SoundKind::Typed,
            shifted: false,
            glyph_class: SECRET,
            rank: 0,
        };
    }
    KeyClickStamp {
        kind: if spacebar {
            SoundKind::Space
        } else {
            SoundKind::Typed
        },
        shifted: shifted && !spacebar,
        glyph_class: typed_glyph_class(typed),
        rank: typed_glyph_rank(typed),
    }
}

/// The keyed seam's `at_ms`.
#[inline]
fn key_sound_at_ms() -> u32 {
    crate::metrics::key_arrival_ms().unwrap_or_else(crate::app_render::input_clock_ms)
}

/// The synth's glyph rank ([`aterm_effects::trail_sound::EventMeta::rank`]).
#[inline]
pub(crate) const fn typed_glyph_rank(typed: Option<char>) -> u8 {
    aterm_effects::trail_sound::typed_glyph_rank(typed)
}

/// Whether an input event carries fresh, discrete intent.
#[inline]
pub(crate) const fn is_present_recovery_stimulus(ev: &InputEvent) -> bool {
    !matches!(
        ev,
        InputEvent::Key {
            event_type: aterm_types::keyboard::KeyEventType::Release,
            ..
        } | InputEvent::MouseMove { .. }
    )
}

/// Test-only observation of the real physical-release routing branch.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PhysicalReleaseTrace {
    Consumed {
        arrival_window: WindowId,
        press_window: WindowId,
    },
    Forwarded {
        arrival_window: WindowId,
        press_window: WindowId,
        session: u64,
        key: aterm_types::keyboard::Key,
        mods: aterm_types::keyboard::Modifiers,
        base_layout: Option<char>,
        event_type: aterm_types::keyboard::KeyEventType,
        delivery: input::Delivery,
    },
    Literal {
        arrival_window: WindowId,
        press_window: WindowId,
        session: u64,
        event: InputEvent,
        repeated: bool,
        delivery: input::Delivery,
    },
    Local {
        arrival_window: WindowId,
        press_window: WindowId,
    },
}

#[cfg(test)]
std::thread_local! {
    static PHYSICAL_RELEASE_TRACE:
        std::cell::RefCell<Option<PhysicalReleaseTrace>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn clear_physical_release_trace() {
    PHYSICAL_RELEASE_TRACE.with(|trace| *trace.borrow_mut() = None);
}

#[cfg(test)]
fn record_physical_release_trace(observed: PhysicalReleaseTrace) {
    PHYSICAL_RELEASE_TRACE.with(|trace| *trace.borrow_mut() = Some(observed));
}

#[cfg(test)]
pub(crate) fn take_physical_release_trace() -> Option<PhysicalReleaseTrace> {
    PHYSICAL_RELEASE_TRACE.with(|trace| trace.borrow_mut().take())
}

/// Compatibility `open prefs|about|update` spellings resolve to durable routes in the
/// process-singleton Settings tab.
const fn native_settings_route_for_aux(
    target: crate::app_introspect::AuxTarget,
) -> Option<crate::native_settings::SettingsRoute> {
    use crate::app_introspect::AuxTarget;
    use crate::native_settings::SettingsRoute;
    match target {
        AuxTarget::Prefs => Some(SettingsRoute::Home),
        AuxTarget::About => Some(SettingsRoute::About),
        AuxTarget::Update => Some(SettingsRoute::SoftwareUpdate),
        AuxTarget::Front
        | AuxTarget::Menu
        | AuxTarget::TabMenu
        | AuxTarget::AutopilotMenu
        | AuxTarget::FactMenu
        | AuxTarget::Overview => None,
    }
}

const fn native_binding_allowed(action: keybinding::Action) -> bool {
    use keybinding::Action;
    matches!(
        action,
        Action::NewTab
            | Action::ReopenClosedTab
            | Action::CloseTab
            | Action::NewWindow
            | Action::NextTab
            | Action::PrevTab
            | Action::SwitchTab(_)
            | Action::SplitVertical
            | Action::SplitHorizontal
            | Action::Paste
            | Action::PasteSelection
            | Action::Find
            | Action::FindNext
            | Action::FindPrev
            | Action::FocusPaneLeft
            | Action::FocusPaneRight
            | Action::FocusPaneUp
            | Action::FocusPaneDown
            | Action::TogglePaneZoom
            | Action::ScrollPageUp
            | Action::ScrollPageDown
            | Action::ScrollLineUp
            | Action::ScrollLineDown
            | Action::ScrollToTop
            | Action::ScrollToBottom
            | Action::ToggleSettings
            | Action::ToggleAbout
            | Action::ToggleMatrixRain
            | Action::ToggleSeriousMode
            | Action::OpenPalette
            | Action::ChooseTheme
            | Action::ToggleFullscreen
    )
}

const _: () = {
    use keybinding::Action;
    assert!(native_binding_allowed(Action::NewTab));
    assert!(native_binding_allowed(Action::ReopenClosedTab));
    assert!(native_binding_allowed(Action::CloseTab));
    assert!(native_binding_allowed(Action::NextTab));
    assert!(native_binding_allowed(Action::SwitchTab(3)));
    assert!(native_binding_allowed(Action::Paste));
    assert!(native_binding_allowed(Action::Find));
    assert!(native_binding_allowed(Action::FindNext));
    assert!(native_binding_allowed(Action::FindPrev));
    assert!(native_binding_allowed(Action::ScrollPageDown));
    assert!(native_binding_allowed(Action::ToggleSettings));
    assert!(native_binding_allowed(Action::ChooseTheme));
    assert!(native_binding_allowed(Action::ToggleFullscreen));
    assert!(!native_binding_allowed(Action::FontIncrease));
    assert!(!native_binding_allowed(Action::FontReset));
    assert!(!native_binding_allowed(Action::JumpPrevPrompt));
    assert!(!native_binding_allowed(Action::JumpNextPrompt));
    assert!(!native_binding_allowed(Action::SelectAll));
    assert!(!native_binding_allowed(Action::Copy));
};

/// Per-session ordered egress serializer.
pub(crate) mod paste_order {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
    use std::sync::{Arc, LazyLock, Mutex, Weak};
    use std::time::Duration;

    use super::{
        DeliveryTicket, InputEvent, OutputEchoTracker, SinkWriter, Terminal, Wake,
        tracked_egress_metered, tracked_egress_ticketed,
    };
    use aterm_core::terminal::ModeMirror;
    use aterm_session::sink::BulkMeter;
    use aterm_window::event_loop::EventLoopProxy;

    const MAX_BYTES: u32 = 32 * 1024 * 1024;
    const MAX_JOBS: u32 = 256;
    const KEY_BYTES: u32 = 64 * 1024;
    const KEY_JOBS: u32 = 32;
    const ONE_JOB: u64 = 1 << 32;

    /// Bounds include the job currently writing.
    #[derive(Clone, Copy)]
    struct Limits {
        bytes: u32,
        jobs: u32,
        paste_bytes: u32,
        paste_jobs: u32,
    }

    const LIMITS: Limits = Limits {
        bytes: MAX_BYTES,
        jobs: MAX_JOBS,
        paste_bytes: MAX_BYTES - KEY_BYTES,
        paste_jobs: MAX_JOBS - KEY_JOBS,
    };

    struct Budget {
        /// High 32 bits: jobs.
        used: AtomicU64,
        limits: Limits,
    }

    impl Budget {
        const fn new(limits: Limits) -> Self {
            Self {
                used: AtomicU64::new(0),
                limits,
            }
        }

        #[cfg_attr(
            any(test, debug_assertions),
            aterm_spec_anchor::refines(
                machine = "ordered_input_admission",
                action = "AdmitPaste",
                project = "paste_order::admission_tests::project"
            )
        )]
        #[cfg_attr(
            any(test, debug_assertions),
            aterm_spec_anchor::refines(
                machine = "ordered_input_admission",
                action = "AdmitInput",
                project = "paste_order::admission_tests::project"
            )
        )]
        #[cfg_attr(
            any(test, debug_assertions),
            aterm_spec_anchor::refines(
                machine = "ordered_input_admission",
                action = "AdmitTiny",
                project = "paste_order::admission_tests::project"
            )
        )]
        fn claim(&self, bytes: u32, paste: bool) -> bool {
            let (byte_cap, job_cap) = if paste {
                (self.limits.paste_bytes, self.limits.paste_jobs)
            } else {
                (self.limits.bytes, self.limits.jobs)
            };
            self.used
                .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                    let retained = used & u64::from(u32::MAX);
                    let jobs = used >> 32;
                    (jobs < u64::from(job_cap)
                        && retained + u64::from(bytes) <= u64::from(byte_cap))
                    .then_some(used + ONE_JOB + u64::from(bytes))
                })
                .is_ok()
        }

        /// Whether a charge of `bytes` fits this queue at all, however empty.
        const fn could_ever_hold(&self, bytes: u32, paste: bool) -> bool {
            bytes
                <= if paste {
                    self.limits.paste_bytes
                } else {
                    self.limits.bytes
                }
        }

        #[cfg_attr(
            any(test, debug_assertions),
            aterm_spec_anchor::refines(
                machine = "ordered_input_admission",
                action = "ReleasePaste",
                project = "paste_order::admission_tests::project"
            )
        )]
        #[cfg_attr(
            any(test, debug_assertions),
            aterm_spec_anchor::refines(
                machine = "ordered_input_admission",
                action = "ReleaseInput",
                project = "paste_order::admission_tests::project"
            )
        )]
        #[cfg_attr(
            any(test, debug_assertions),
            aterm_spec_anchor::refines(
                machine = "ordered_input_admission",
                action = "ReleaseTiny",
                project = "paste_order::admission_tests::project"
            )
        )]
        fn release(&self, bytes: u32) {
            let prior = self
                .used
                .fetch_sub(ONE_JOB + u64::from(bytes), Ordering::AcqRel);
            debug_assert!(prior >> 32 > 0 && prior & u64::from(u32::MAX) >= u64::from(bytes));
        }
    }

    /// Owns the admission until all of a job's data is dropped.
    struct Reservation {
        budget: Arc<Budget>,
        sink: Arc<SinkWriter>,
        bytes: u32,
        meter: Option<Arc<BulkMeter>>,
    }

    impl Drop for Reservation {
        fn drop(&mut self) {
            // A receiver can unwind with accepted jobs still queued. Their
            // watches must finish too; preserve an already published verdict.
            if let Some(meter) = &self.meter
                && matches!(
                    meter.progress().state,
                    aterm_session::sink::BulkState::Queued
                        | aterm_session::sink::BulkState::Writing
                )
            {
                meter.settle_unmetered(false);
            }
            self.budget.release(self.bytes);
            self.sink.retire_ordered_egress();
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum Rejected {
        /// The queue is full now.
        Full,
        /// The input's charge (owned capacity included) exceeds the queue even when it is empty.
        TooLarge,
        Unavailable,
    }

    fn retained_bytes(ev: &InputEvent) -> Option<u32> {
        let capacity = match ev {
            InputEvent::Text(text) | InputEvent::Paste(text, _) => text.capacity(),
            InputEvent::KeySequence(bytes) => bytes.capacity(),
            _ => 0,
        };
        capacity
            .checked_add(std::mem::size_of::<Job>())?
            .try_into()
            .ok()
    }

    /// A deferred egress.
    struct Job {
        term: Arc<Mutex<Terminal>>,
        modes: Arc<ModeMirror>,
        sink: Arc<SinkWriter>,
        /// The session's echo tracker ([`tracked_egress_metered`]).
        echo: Option<Arc<OutputEchoTracker>>,
        ev: InputEvent,
        /// What this write's completion re-stamps on the cursor engines.
        ticket: Option<DeliveryTicket>,
        /// The key-arrival stamp claimed at enqueue.
        key_ns: u64,
        /// A visible press queued behind this sink's paste, never the paste or a key release.
        hot_key: Option<(u64, Option<EventLoopProxy<Wake>>)>,
        /// A large paste's progress and stop.
        meter: Option<Arc<BulkMeter>>,
        completion: Option<crate::control_input_ack::InputCompletion>,
        /// Who waits for this write's own receipt ([`Written`]).
        written: Option<SyncSender<crate::input::EgressReceipt>>,
        /// Last field: payloads above are dropped before capacity is returned.
        _reservation: Reservation,
    }

    /// The receipt of a write handed to the session's ordered writer.
    pub(crate) struct Written(Receiver<crate::input::EgressReceipt>);

    impl Written {
        /// The write's receipt once the writer has run it, waiting at most `bound`.
        pub(crate) fn wait(
            &self,
            bound: Duration,
        ) -> Result<crate::input::EgressReceipt, RecvTimeoutError> {
            self.0.recv_timeout(bound)
        }

        /// Test-only: a receipt and the sender the writer would answer it on.
        #[cfg(test)]
        #[cfg_attr(not(unix), allow(dead_code))]
        pub(crate) fn for_test() -> (SyncSender<crate::input::EgressReceipt>, Written) {
            let (tx, rx) = sync_channel(1);
            (tx, Written(rx))
        }
    }

    const UI_WRITES_CAN_BLOCK: bool = cfg!(windows);

    #[cfg(test)]
    thread_local! {
        static CONPTY_ROUTE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// The platform's routing fact ([`UI_WRITES_CAN_BLOCK`]).
    fn ui_writes_can_block() -> bool {
        #[cfg(test)]
        if CONPTY_ROUTE.with(std::cell::Cell::get) {
            return true;
        }
        UI_WRITES_CAN_BLOCK
    }

    /// Test-only: routes this thread's keys and reports as on Windows until the guard drops.
    #[cfg(test)]
    pub(crate) fn force_conpty_route_for_test() -> ConptyRoute {
        CONPTY_ROUTE.with(|route| route.set(true));
        ConptyRoute(())
    }

    #[cfg(test)]
    pub(crate) struct ConptyRoute(());

    #[cfg(test)]
    impl Drop for ConptyRoute {
        fn drop(&mut self) {
            CONPTY_ROUTE.with(|route| route.set(false));
        }
    }

    /// Test-only: the routes a test on this host can drive.
    #[cfg(test)]
    pub(crate) fn routes_for_test() -> &'static [bool] {
        if UI_WRITES_CAN_BLOCK {
            &[true]
        } else {
            &[false, true]
        }
    }

    #[cfg(test)]
    thread_local! {
        static SUBMITTED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }

    /// Test-only: how many jobs this thread has handed to an ordered writer.
    #[cfg(test)]
    pub(crate) fn submitted_on_this_thread_for_test() -> u64 {
        SUBMITTED.with(std::cell::Cell::get)
    }

    /// One session's serializer.
    struct Serializer {
        tx: SyncSender<Job>,
        sink: Weak<SinkWriter>,
        budget: Arc<Budget>,
    }

    static REG: LazyLock<Mutex<HashMap<i32, Serializer>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    /// A queued key is the only completed write that can renew echo-present priority.
    pub(super) fn completed_queued_key_wake(
        receipt: crate::input::EgressReceipt,
        session: Option<u64>,
    ) -> Option<Wake> {
        let session =
            session.filter(|_| receipt.fully_accepted_nonempty() && receipt.is_direct())?;
        Some(Wake::QueuedKeyDelivered {
            session,
            at: std::time::Instant::now(),
        })
    }

    /// The writer thread body.
    fn run(rx: Receiver<Job>, sink: Weak<SinkWriter>, idle_check: Duration) {
        loop {
            let job = match rx.recv_timeout(idle_check) {
                Ok(job) => job,
                Err(RecvTimeoutError::Timeout) if sink.strong_count() == 0 => break,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            };
            run_one(job);
        }
    }

    fn run_one(mut job: Job) {
        let receipt = match &job.echo {
            Some(echo) => tracked_egress_metered(
                &job.term,
                &job.modes,
                &job.sink,
                echo,
                &job.ev,
                crate::input::EgressMode::Backpressured,
                job.ticket,
                job.meter.as_deref(),
            ),
            None => crate::input::seam_egress_receipt(
                &job.term,
                &job.modes,
                &job.sink,
                &job.ev,
                crate::input::EgressMode::Backpressured,
            ),
        };
        if let Some(completion) = job.completion.take() {
            completion.finish(receipt);
        }
        if let Some(written) = job.written.take() {
            let _ = written.try_send(receipt);
        }
        crate::metrics::note_pty_write_at(job.key_ns);
        if let Some((session, _)) = job.hot_key
            && receipt.fully_accepted_nonempty()
        {
            crate::echo_rtt::arm_deferred(session, &job.sink);
        }
        if let Some((session, proxy)) = job.hot_key.take()
            && let Some(wake) = completed_queued_key_wake(receipt, Some(session))
            && let Some(proxy) = proxy
        {
            let _ = proxy.send_event(wake);
        }
    }

    #[cfg(all(test, unix))]
    mod completion_tests {
        use super::*;
        use crate::control_input_ack::{InputMark, testing};

        #[test]
        fn only_completed_nonempty_jobs_mark_the_exact_admitted_log() {
            let (handle, mut reader) = testing::owned_handle(73);
            {
                let mut timeline = handle.ctx.timeline.lock().unwrap();
                let at = timeline.slog().now();
                timeline.slog_mut().open_session(None, None, at);
            }
            let caller = |word: &str| {
                Some(crate::typed_by::Caller::new(crate::typed_by::Who::Word(
                    word.to_owned(),
                )))
            };
            let credited = || handle.ctx.typed.last().map(|t| t.by.words());
            let mark = InputMark::at_epoch(
                &handle.ctx,
                73,
                aterm_slog::Actor::Bridge,
                crate::session_log::InputVerb::Feed,
                None,
                false,
            )
            .typed_by(&handle.ctx, caller("first"));
            let budget = Arc::new(Budget::new(LIMITS));
            let job = |event: InputEvent, completion| {
                let bytes = retained_bytes(&event).expect("tiny job");
                assert!(budget.claim(bytes, false));
                handle.ctx.sink.claim_ordered_egress();
                Job {
                    term: handle.term.clone(),
                    modes: handle.ctx.modes.clone(),
                    sink: handle.ctx.sink.clone(),
                    echo: Some(handle.ctx.output_echo.clone()),
                    ev: event,
                    ticket: None,
                    key_ns: 0,
                    hot_key: None,
                    meter: None,
                    completion: Some(completion),
                    written: None,
                    _reservation: Reservation {
                        budget: budget.clone(),
                        sink: handle.ctx.sink.clone(),
                        bytes,
                        meter: None,
                    },
                }
            };
            let (tx, rx) = sync_channel(1);
            assert!(
                tx.try_send(job(InputEvent::KeySequence(vec![b'Q']), mark.completion()))
                    .is_ok()
            );
            assert_eq!(
                testing::input_count(&handle),
                0,
                "admission is not acceptance"
            );
            assert_eq!(credited(), None, "queue admission gives no credit");
            run_one(rx.try_recv().expect("owned admitted job"));
            assert_eq!(credited().as_deref(), Some("first"));
            assert_eq!(testing::byte(&mut reader), b'Q');
            assert_eq!(
                testing::input_count(&handle),
                1,
                "ordinary actual completion marks"
            );
            assert_eq!(budget.used.load(Ordering::Acquire), 0);
            let cancelled = InputMark::at_epoch(
                &handle.ctx,
                73,
                aterm_slog::Actor::Harness,
                crate::session_log::InputVerb::Key,
                None,
                false,
            )
            .typed_by(&handle.ctx, caller("cancelled"));
            drop(job(
                InputEvent::KeySequence(vec![b'X']),
                cancelled.completion(),
            ));
            assert_eq!(budget.used.load(Ordering::Acquire), 0);
            assert_eq!(handle.ctx.sink.ordered_egress_count(), 0);
            assert_eq!(
                credited().as_deref(),
                Some("first"),
                "unrun job kept no credit"
            );
            let refused = InputMark::at_epoch(
                &handle.ctx,
                73,
                aterm_slog::Actor::Ctl,
                crate::session_log::InputVerb::Key,
                None,
                false,
            )
            .typed_by(&handle.ctx, caller("next"));
            for receipt in [
                crate::input::EgressReceipt::deferred_full(),
                crate::input::EgressReceipt::queue_full(),
                crate::input::EgressReceipt::unwritten(crate::input::Egress::Reported(
                    crate::input::Delivery::Full,
                )),
            ] {
                refused.completion().finish(receipt);
            }
            run_one(job(
                InputEvent::KeySequence(Vec::new()),
                refused.completion(),
            ));
            assert_eq!(
                testing::input_count(&handle),
                1,
                "empty, deferred, refused and cancelled input is unmarked"
            );
            assert_eq!(
                credited().as_deref(),
                Some("first"),
                "empty/deferred/refused kept no credit"
            );
            run_one(job(
                InputEvent::KeySequence(vec![b'R']),
                refused.completion(),
            ));
            assert_eq!(testing::byte(&mut reader), b'R');
            assert_eq!(budget.used.load(Ordering::Acquire), 0);
            assert_eq!(handle.ctx.sink.ordered_egress_count(), 0);
            assert_eq!(
                testing::input_count(&handle),
                2,
                "CTL_INPUT_MARK_REQUIRES_COMPLETED_NONEMPTY_RECEIPT"
            );
            assert_eq!(credited().as_deref(), Some("next"));
            assert!(testing::paste(&handle, &mark, "Z"));
            assert_eq!(testing::byte(&mut reader), b'Z');
            assert_eq!(
                credited().as_deref(),
                Some("next"),
                "one mark credits only its first acceptance"
            );
            assert_eq!(testing::input_count(&handle), 2);
            let stale = InputMark::at_epoch(
                &handle.ctx,
                73,
                aterm_slog::Actor::Harness,
                crate::session_log::InputVerb::Send,
                None,
                false,
            )
            .typed_by(&handle.ctx, caller("stale"));
            {
                let mut timeline = handle.ctx.timeline.lock().unwrap();
                let old = timeline.slog().log().ep();
                let at = timeline.slog().now();
                timeline.slog_mut().adopt(
                    crate::session_log::LogAdopt {
                        ids: Some(crate::session_log::LogCarry {
                            ep: old,
                            ids: aterm_slog::IdCounters::default(),
                        }),
                        seq: 0,
                        lines: None,
                        keeper_ep: None,
                        mirror: None,
                    },
                    false,
                    None,
                    at,
                );
                timeline.slog_mut().open_session(None, None, at);
                assert_ne!(timeline.slog().log().ep(), old, "ordinary epoch transition");
            }
            run_one(job(InputEvent::KeySequence(vec![b'E']), stale.completion()));
            assert_eq!(
                testing::byte(&mut reader),
                b'E',
                "acceptance itself is unchanged"
            );
            assert_eq!(
                testing::input_count(&handle),
                0,
                "an old request cannot mark a new log epoch"
            );
            assert_eq!(
                credited().as_deref(),
                Some("next"),
                "old epoch cannot credit a newer one"
            );
            let current = InputMark::at_epoch(
                &handle.ctx,
                73,
                aterm_slog::Actor::Harness,
                crate::session_log::InputVerb::Send,
                None,
                false,
            )
            .typed_by(&handle.ctx, caller("current"));
            run_one(job(
                InputEvent::KeySequence(vec![b'N']),
                current.completion(),
            ));
            assert_eq!(testing::byte(&mut reader), b'N');
            assert_eq!(credited().as_deref(), Some("current"));
            assert_eq!(
                testing::input_count(&handle),
                1,
                "a current-epoch receipt remains observable"
            );
            assert_eq!(budget.used.load(Ordering::Acquire), 0);
            assert_eq!(handle.ctx.sink.ordered_egress_count(), 0);
        }
    }

    /// The FIFO sender for `master`, spawning the writer thread on first use.
    fn writer_for(
        reg: &mut HashMap<i32, Serializer>,
        master: i32,
        sink: &Arc<SinkWriter>,
    ) -> Option<(SyncSender<Job>, Arc<Budget>)> {
        writer_for_limits(reg, master, sink, LIMITS)
    }

    fn writer_for_limits(
        reg: &mut HashMap<i32, Serializer>,
        master: i32,
        sink: &Arc<SinkWriter>,
        limits: Limits,
    ) -> Option<(SyncSender<Job>, Arc<Budget>)> {
        // A slot filed for another sink is one whose borrowed fd closed while it lived on, the
        // number since reused: this sink takes the number over.
        if let Some(s) = reg.get(&master)
            && std::ptr::eq(s.sink.as_ptr(), Arc::as_ptr(sink))
        {
            return Some((s.tx.clone(), Arc::clone(&s.budget)));
        }
        let (tx, rx) = sync_channel::<Job>(limits.jobs as usize);
        let budget = Arc::new(Budget::new(limits));
        let weak_sink = Arc::downgrade(sink);
        crate::qos::spawn_lane_thread(
            "aterm-egress-order",
            crate::qos::Role::Interactive,
            move || {
                run(rx, weak_sink, Duration::from_secs(30));
            },
        )
        .ok()?;
        reg.insert(
            master,
            Serializer {
                tx: tx.clone(),
                sink: Arc::downgrade(sink),
                budget: Arc::clone(&budget),
            },
        );
        Some((tx, budget))
    }

    /// Whether this sink's egress must be ordered behind a submitted paste or key.
    pub(crate) fn is_ordering(sink: &SinkWriter) -> bool {
        sink.ordered_egress_count() > 0
    }

    /// Wait until this sink's ordered egress has drained ([`aterm_spec::wait::MUST_HAPPEN`]).
    #[cfg(test)]
    pub(crate) fn await_drain_for_test(sink: &SinkWriter) {
        let _ = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            std::time::Duration::from_millis(1),
            || (!is_ordering(sink)).then_some(()),
        );
    }

    /// The update handoff's cold drain gate, by master.
    #[cfg(unix)]
    pub(crate) fn is_master_ordering_for_handoff(master: i32) -> bool {
        REG.lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&master)
            .and_then(|serializer| serializer.sink.upgrade())
            .is_some_and(|sink| sink.ordered_egress_count() > 0)
    }

    /// Test seam: keep the cold writer registry locked while another session makes its hot-path
    /// ordering decision.
    #[cfg(all(test, unix))]
    pub(crate) fn hold_registry_for_test() -> impl Drop {
        REG.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Enqueue `ev` onto `master`'s FIFO so it reaches the PTY in submission order.
    #[cfg(all(test, unix))]
    pub(super) fn enqueue(
        term: &Arc<Mutex<Terminal>>,
        modes: &Arc<ModeMirror>,
        sink: &Arc<SinkWriter>,
        echo: &Arc<OutputEchoTracker>,
        ev: InputEvent,
        ticket: Option<DeliveryTicket>,
        hot_key: Option<(u64, Option<&EventLoopProxy<Wake>>)>,
    ) -> Result<(), Rejected> {
        enqueue_metered(term, modes, sink, echo, ev, ticket, hot_key, None)
    }

    /// [`enqueue`] for a large paste with its meter.
    #[allow(
        clippy::too_many_arguments,
        reason = "enqueue's handles plus the paste's meter, which rides the FIFO job"
    )]
    #[cfg(all(test, unix))]
    pub(super) fn enqueue_metered(
        term: &Arc<Mutex<Terminal>>,
        modes: &Arc<ModeMirror>,
        sink: &Arc<SinkWriter>,
        echo: &Arc<OutputEchoTracker>,
        ev: InputEvent,
        ticket: Option<DeliveryTicket>,
        hot_key: Option<(u64, Option<&EventLoopProxy<Wake>>)>,
        meter: Option<Arc<BulkMeter>>,
    ) -> Result<(), Rejected> {
        enqueue_metered_observed(term, modes, sink, echo, ev, ticket, hot_key, meter, None)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the receipt follows the existing input capability handles"
    )]
    pub(super) fn enqueue_metered_observed(
        term: &Arc<Mutex<Terminal>>,
        modes: &Arc<ModeMirror>,
        sink: &Arc<SinkWriter>,
        echo: &Arc<OutputEchoTracker>,
        ev: InputEvent,
        ticket: Option<DeliveryTicket>,
        hot_key: Option<(u64, Option<&EventLoopProxy<Wake>>)>,
        meter: Option<Arc<BulkMeter>>,
        completion: Option<crate::control_input_ack::InputCompletion>,
    ) -> Result<(), Rejected> {
        submit(
            term,
            modes,
            sink,
            Some(echo),
            ev,
            ticket,
            hot_key,
            meter,
            completion,
            None,
        )
    }

    /// The one submission body behind `enqueue_metered` and the report route.
    #[allow(
        clippy::too_many_arguments,
        reason = "enqueue_metered's handles, with the tracker optional for an encoded report"
    )]
    fn submit(
        term: &Arc<Mutex<Terminal>>,
        modes: &Arc<ModeMirror>,
        sink: &Arc<SinkWriter>,
        echo: Option<&Arc<OutputEchoTracker>>,
        ev: InputEvent,
        ticket: Option<DeliveryTicket>,
        hot_key: Option<(u64, Option<&EventLoopProxy<Wake>>)>,
        meter: Option<Arc<BulkMeter>>,
        completion: Option<crate::control_input_ack::InputCompletion>,
        written: Option<SyncSender<crate::input::EgressReceipt>>,
    ) -> Result<(), Rejected> {
        let bytes = retained_bytes(&ev).ok_or(Rejected::TooLarge)?;
        let paste = matches!(ev, InputEvent::Paste(..)) || echo.is_none();
        let master = sink.master();
        let (tx, budget) = {
            let mut reg = REG.lock().unwrap_or_else(|p| p.into_inner());
            reg.retain(|_, s| s.sink.strong_count() > 0);
            match writer_for(&mut reg, master, sink) {
                Some(w) => w,
                None => return Err(Rejected::Unavailable),
            }
        };
        if !budget.claim(bytes, paste) {
            return Err(if budget.could_ever_hold(bytes, paste) {
                Rejected::Full
            } else {
                Rejected::TooLarge
            });
        }
        sink.claim_ordered_egress();
        let key_ns = if matches!(ev, InputEvent::Paste(..)) || echo.is_none() {
            0
        } else {
            crate::metrics::take_key_arrival()
        };
        let job = Job {
            term: term.clone(),
            modes: modes.clone(),
            sink: sink.clone(),
            echo: echo.cloned(),
            ev,
            key_ns,
            ticket,
            hot_key: hot_key.map(|(session, proxy)| (session, proxy.cloned())),
            meter: meter.clone(),
            completion,
            written,
            _reservation: Reservation {
                budget,
                sink: Arc::clone(sink),
                bytes,
                meter,
            },
        };
        match tx.try_send(job) {
            Ok(()) => {
                crate::echo_rtt::note_deferred_write();
                #[cfg(test)]
                SUBMITTED.with(|n| n.set(n.get() + 1));
                Ok(())
            }
            Err(error) => {
                let (reason, job) = match error {
                    std::sync::mpsc::TrySendError::Full(job) => (Rejected::Full, job),
                    std::sync::mpsc::TrySendError::Disconnected(job) => {
                        (Rejected::Unavailable, job)
                    }
                };
                if reason == Rejected::Unavailable {
                    let mut reg = REG.lock().unwrap_or_else(|p| p.into_inner());
                    if reg
                        .get(&master)
                        .is_some_and(|entry| Arc::ptr_eq(&entry.budget, &job._reservation.budget))
                    {
                        reg.remove(&master);
                    }
                }
                drop(job);
                Err(reason)
            }
        }
    }

    /// The shared ordered/inline tail.
    pub(super) fn ordered_or_inline(
        term: &Arc<Mutex<Terminal>>,
        modes: &Arc<ModeMirror>,
        sink: &Arc<SinkWriter>,
        echo: &Arc<OutputEchoTracker>,
        ev: &InputEvent,
        ticket: Option<DeliveryTicket>,
        hot_key: Option<(u64, Option<&EventLoopProxy<Wake>>)>,
    ) -> (crate::input::EgressReceipt, bool) {
        ordered_or_inline_observed(term, modes, sink, echo, ev, ticket, hot_key, None)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the receipt follows the existing input capability handles"
    )]
    pub(super) fn ordered_or_inline_observed(
        term: &Arc<Mutex<Terminal>>,
        modes: &Arc<ModeMirror>,
        sink: &Arc<SinkWriter>,
        echo: &Arc<OutputEchoTracker>,
        ev: &InputEvent,
        ticket: Option<DeliveryTicket>,
        hot_key: Option<(u64, Option<&EventLoopProxy<Wake>>)>,
        completion: Option<crate::control_input_ack::InputCompletion>,
    ) -> (crate::input::EgressReceipt, bool) {
        let queue =
            ui_writes_can_block() || is_ordering(sink) || matches!(ev, InputEvent::Paste(..));
        queued_or_inline_observed(
            queue, term, modes, sink, echo, ev, ticket, hot_key, completion,
        )
    }

    /// [`ordered_or_inline`] with the platform's routing fact explicit.
    #[allow(
        clippy::too_many_arguments,
        reason = "ordered_or_inline's handles plus the one routing fact it is parameterised on"
    )]
    #[cfg(all(test, unix))]
    pub(super) fn ordered_or_inline_routed(
        ui_writes_can_block: bool,
        term: &Arc<Mutex<Terminal>>,
        modes: &Arc<ModeMirror>,
        sink: &Arc<SinkWriter>,
        echo: &Arc<OutputEchoTracker>,
        ev: &InputEvent,
        ticket: Option<DeliveryTicket>,
        hot_key: Option<(u64, Option<&EventLoopProxy<Wake>>)>,
    ) -> (crate::input::EgressReceipt, bool) {
        let queue = ui_writes_can_block || is_ordering(sink) || matches!(ev, InputEvent::Paste(..));
        queued_or_inline(queue, term, modes, sink, echo, ev, ticket, hot_key)
    }

    /// A control lane's write into a session not on screen.
    pub(crate) fn unparked_observed(
        term: &Arc<Mutex<Terminal>>,
        modes: &Arc<ModeMirror>,
        sink: &Arc<SinkWriter>,
        echo: &Arc<OutputEchoTracker>,
        ev: &InputEvent,
        completion: Option<crate::control_input_ack::InputCompletion>,
    ) -> (crate::input::EgressReceipt, Option<Written>) {
        if !(ui_writes_can_block() || is_ordering(sink)) {
            let inline = queued_or_inline_observed(
                false, term, modes, sink, echo, ev, None, None, completion,
            );
            return (inline.0, None);
        }
        let (written, receipt) = sync_channel(1);
        match submit(
            term,
            modes,
            sink,
            Some(echo),
            ev.clone(),
            None,
            None,
            None,
            completion,
            Some(written),
        ) {
            Ok(()) => (
                crate::input::EgressReceipt::deferred_full(),
                Some(Written(receipt)),
            ),
            Err(_) => (crate::input::EgressReceipt::queue_full(), None),
        }
    }

    /// The tail [`ordered_or_inline_routed`] and [`unparked_observed`] share.
    #[allow(
        clippy::too_many_arguments,
        reason = "ordered_or_inline's handles plus the routing decision already made"
    )]
    #[cfg(all(test, unix))]
    fn queued_or_inline(
        queue: bool,
        term: &Arc<Mutex<Terminal>>,
        modes: &Arc<ModeMirror>,
        sink: &Arc<SinkWriter>,
        echo: &Arc<OutputEchoTracker>,
        ev: &InputEvent,
        ticket: Option<DeliveryTicket>,
        hot_key: Option<(u64, Option<&EventLoopProxy<Wake>>)>,
    ) -> (crate::input::EgressReceipt, bool) {
        queued_or_inline_observed(queue, term, modes, sink, echo, ev, ticket, hot_key, None)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the receipt follows the existing input capability handles"
    )]
    fn queued_or_inline_observed(
        queue: bool,
        term: &Arc<Mutex<Terminal>>,
        modes: &Arc<ModeMirror>,
        sink: &Arc<SinkWriter>,
        echo: &Arc<OutputEchoTracker>,
        ev: &InputEvent,
        ticket: Option<DeliveryTicket>,
        hot_key: Option<(u64, Option<&EventLoopProxy<Wake>>)>,
        completion: Option<crate::control_input_ack::InputCompletion>,
    ) -> (crate::input::EgressReceipt, bool) {
        if queue {
            match enqueue_metered_observed(
                term,
                modes,
                sink,
                echo,
                ev.clone(),
                ticket,
                hot_key,
                None,
                completion,
            ) {
                Ok(()) => (crate::input::EgressReceipt::deferred_full(), false),
                Err(_) => (crate::input::EgressReceipt::queue_full(), false),
            }
        } else {
            let receipt = tracked_egress_ticketed(
                term,
                modes,
                sink,
                echo,
                ev,
                crate::input::EgressMode::Interactive,
                None,
            );
            if let Some(completion) = completion {
                completion.finish(receipt);
            }
            (receipt, true)
        }
    }

    /// The report egress.
    pub(crate) fn report_ordered_or_inline(
        term: &Arc<Mutex<Terminal>>,
        modes: &Arc<ModeMirror>,
        sink: &Arc<SinkWriter>,
        ev: &InputEvent,
    ) -> crate::input::EgressReceipt {
        report_ordered_or_inline_routed(ui_writes_can_block(), term, modes, sink, ev)
    }

    /// [`report_ordered_or_inline`] with the platform's routing fact explicit.
    pub(super) fn report_ordered_or_inline_routed(
        ui_writes_can_block: bool,
        term: &Arc<Mutex<Terminal>>,
        modes: &Arc<ModeMirror>,
        sink: &Arc<SinkWriter>,
        ev: &InputEvent,
    ) -> crate::input::EgressReceipt {
        report_route(ui_writes_can_block, term, modes, sink, ev, false).0
    }

    /// [`report_ordered_or_inline`] for a control lane's mouse report into a session not on screen.
    pub(crate) fn report_unparked(
        term: &Arc<Mutex<Terminal>>,
        modes: &Arc<ModeMirror>,
        sink: &Arc<SinkWriter>,
        ev: &InputEvent,
    ) -> (crate::input::EgressReceipt, Option<Written>) {
        report_route(ui_writes_can_block(), term, modes, sink, ev, true)
    }

    /// The report route's one body.
    fn report_route(
        ui_writes_can_block: bool,
        term: &Arc<Mutex<Terminal>>,
        modes: &Arc<ModeMirror>,
        sink: &Arc<SinkWriter>,
        ev: &InputEvent,
        written: bool,
    ) -> (crate::input::EgressReceipt, Option<Written>) {
        if !(ui_writes_can_block || is_ordering(sink)) {
            let receipt = crate::input::seam_egress_receipt(
                term,
                modes,
                sink,
                ev,
                crate::input::EgressMode::Interactive,
            );
            return (receipt, None);
        }
        let (egress, bytes) = crate::input::seam_encode(term, modes, sink, ev);
        if bytes.is_empty() {
            return (crate::input::EgressReceipt::unwritten(egress), None);
        }
        let (tx, rx) = if written {
            let (tx, rx) = sync_channel(1);
            (Some(tx), Some(Written(rx)))
        } else {
            (None, None)
        };
        let frame = InputEvent::KeySequence(bytes);
        match submit(term, modes, sink, None, frame, None, None, None, None, tx) {
            Ok(()) => (crate::input::EgressReceipt::deferred_full(), rx),
            Err(_) => (crate::input::EgressReceipt::queue_full(), None),
        }
    }

    /// Test-only: holds this sink's FIFO occupied ([`ordered_or_inline`]).
    #[cfg(all(test, unix))]
    pub(crate) struct OrderingPin {
        sink: Arc<SinkWriter>,
    }

    #[cfg(all(test, unix))]
    impl Drop for OrderingPin {
        fn drop(&mut self) {
            self.sink.retire_ordered_egress();
        }
    }

    #[cfg(all(test, unix))]
    pub(crate) fn pin_ordering_for_test(sink: &Arc<SinkWriter>) -> OrderingPin {
        let master = sink.master();
        {
            let mut reg = REG.lock().unwrap_or_else(|p| p.into_inner());
            reg.retain(|_, s| s.sink.strong_count() > 0);
            writer_for(&mut reg, master, sink).expect("egress-order writer thread");
        };
        sink.claim_ordered_egress();
        OrderingPin { sink: sink.clone() }
    }

    /// Test-only: the jobs and retained bytes `sink`'s serializer holds admitted now.
    #[cfg(all(test, unix))]
    pub(crate) fn admitted_for_test(sink: &Arc<SinkWriter>) -> (u64, u64) {
        REG.lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&sink.master())
            .filter(|serializer| std::ptr::eq(serializer.sink.as_ptr(), Arc::as_ptr(sink)))
            .map_or((0, 0), |serializer| {
                let used = serializer.budget.used.load(Ordering::Acquire);
                (used >> 32, used & u64::from(u32::MAX))
            })
    }

    /// Test-only: the admission's limits.
    #[cfg(all(test, unix))]
    pub(crate) const fn limits_for_test() -> (u32, u32, u32, u32) {
        (
            LIMITS.bytes,
            LIMITS.jobs,
            LIMITS.paste_bytes,
            LIMITS.paste_jobs,
        )
    }

    /// Exercises the real channel-rejection arms without a stalled child or large payloads.
    #[cfg(all(test, unix))]
    pub(crate) fn reject_admission_for_test(
        sink: &Arc<SinkWriter>,
        unavailable: bool,
    ) -> impl Drop {
        struct Rejection {
            sink: Arc<SinkWriter>,
            budget: Arc<Budget>,
            previous: Option<Serializer>,
            _receiver: Option<Receiver<Job>>,
        }

        impl Drop for Rejection {
            fn drop(&mut self) {
                let mut reg = REG.lock().unwrap_or_else(|p| p.into_inner());
                let master = self.sink.master();
                if reg
                    .get(&master)
                    .is_none_or(|entry| Arc::ptr_eq(&entry.budget, &self.budget))
                {
                    reg.remove(&master);
                    if let Some(previous) = self.previous.take() {
                        reg.insert(master, previous);
                    }
                }
            }
        }

        let (tx, rx) = sync_channel(0);
        let receiver = if unavailable { None } else { Some(rx) };
        let budget = Arc::new(Budget::new(LIMITS));
        let mut reg = REG.lock().unwrap_or_else(|p| p.into_inner());
        reg.retain(|_, entry| entry.sink.strong_count() > 0);
        let previous = reg.insert(
            sink.master(),
            Serializer {
                tx,
                sink: Arc::downgrade(sink),
                budget: Arc::clone(&budget),
            },
        );
        Rejection {
            sink: Arc::clone(sink),
            budget,
            previous,
            _receiver: receiver,
        }
    }

    #[cfg(test)]
    mod admission_tests;

    #[test]
    fn closed_sink_retires_idle_writer_without_another_paste() {
        let (tx, rx) = sync_channel::<Job>(1);
        let sink = Arc::new(SinkWriter::new(-1));
        let weak_sink = Arc::downgrade(&sink);
        let writer = std::thread::spawn(move || run(rx, weak_sink, Duration::from_millis(10)));

        drop(sink);
        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while !writer.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let exited_with_sender_live = writer.is_finished();
        drop(tx);
        writer.join().expect("idle egress writer");
        assert!(exited_with_sender_live);
    }
}

/// The input/egress contract, driven through the real ordered writer.
#[cfg(all(test, unix))]
mod egress_contract_tests;

/// The output → redraw-request span and the IME composition clock
/// (`app_input/input_legs_tests.rs`).
#[cfg(all(test, unix))]
mod input_legs_tests;

/// An input method's commit holding a control character is a paste
/// (`app_input/ime_commit_paste_tests.rs`).
#[cfg(all(test, unix))]
mod ime_commit_paste_tests;

#[cfg(test)]
#[path = "app_input_paste_order_sink_isolation_tests.rs"]
mod paste_order_sink_isolation_tests;

/// Map the seam's [`input::Egress`] to the reply-bearing [`InputOutcome`].
const fn egress_to_outcome(e: input::Egress) -> InputOutcome {
    match e {
        input::Egress::Reported(input::Delivery::Full) | input::Egress::TrackingOff { .. } => {
            InputOutcome::Ok
        }
        input::Egress::Reported(_) => InputOutcome::WriteFailed,
    }
}

const fn rejected_outcome(rejected: paste_order::Rejected) -> InputOutcome {
    match rejected {
        paste_order::Rejected::Full => InputOutcome::QueueFull,
        paste_order::Rejected::TooLarge | paste_order::Rejected::Unavailable => {
            InputOutcome::WriteFailed
        }
    }
}

/// [`egress_to_outcome`] for a whole receipt.
pub(crate) fn receipt_outcome(receipt: input::EgressReceipt) -> InputOutcome {
    if receipt.input_queue_full() && receipt.accepted_order().is_none() {
        InputOutcome::QueueFull
    } else {
        egress_to_outcome(receipt.egress)
    }
}

/// Releases (or stacks onto) the elastic-overscroll bounce for `excess_px` of motion the
/// history-end clamp ate (+ past the top, − past the live bottom).
fn release_overscroll(
    ws: &mut crate::WindowState,
    excess_px: i64,
    cell_h: i64,
    tick_interval: std::time::Duration,
    now: std::time::Instant,
) {
    let impulse = -(crate::scroll_motion::overscroll_resist(excess_px) as f64);
    let max_px = f64::from((cell_h as i32 - 1).max(0));
    if impulse != 0.0 && max_px > 0.0 {
        match ws.overscroll.as_mut() {
            Some(sp) => {
                sp.add_impulse(impulse, max_px, now);
                if sp.next_tick() <= now {
                    sp.set_next_tick(now + tick_interval);
                }
            }
            None => {
                let mut sp = crate::scroll_motion::OverscrollSpring::new(
                    impulse.clamp(-max_px, max_px),
                    now,
                );
                sp.set_next_tick(now + tick_interval);
                ws.overscroll = Some(sp);
            }
        }
        if let Some(sp) = ws.overscroll.as_ref() {
            ws.scroll_frac_px = sp.sample(now).0.round() as i32;
        }
    }
}

/// How many viewport lines a wheel gesture of `notch_lines` moves.
fn wheel_viewport_lines(notch_lines: i32, page_rows: u16, detent: bool) -> i32 {
    input::wheel_platform_lines(input::WheelPlatform::Host, notch_lines, page_rows, detent)
}

/// The modifier-independent (unshifted base) logical key of a winit event, for the keybinding
/// lookup.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub(crate) fn base_logical_key(ev: &KeyEvent) -> Key {
    use aterm_window::platform::modifier_supplement::KeyEventExtModifierSupplement;
    if ev.location == aterm_window::keyboard::KeyLocation::Numpad {
        return ev.logical_key.clone();
    }
    ev.key_without_modifiers()
}

/// Fallback for platforms without the modifier-supplement extension.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub(crate) fn base_logical_key(ev: &KeyEvent) -> Key {
    ev.logical_key.clone()
}

/// The key a hardcoded field chord (`^A`, `^K`, Alt+C, ...) reads.
fn field_chord_key(ev: &KeyEvent, mods: ModifiersState) -> Key {
    let base = base_logical_key(ev);
    match keybinding::chord_layout_twin(&base, ev.physical_key, mods) {
        Some(twin) => Key::Character(aterm_window::keyboard::SmolStr::new(
            twin.encode_utf8(&mut [0; 4]),
        )),
        None => base,
    }
}

/// Classifies one keystroke as a single-line text-field edit, or `None`.
pub(crate) fn field_edit_action(
    mods: ModifiersState,
    ev: &KeyEvent,
) -> Option<crate::app_search::SearchEdit> {
    use aterm_types::keyboard::NamedKey as TNamed;
    let base = match field_chord_key(ev, mods) {
        Key::Character(c) => {
            let mut chars = c.chars();
            match (chars.next(), chars.next()) {
                (Some(only), None) => Some(only),
                _ => None,
            }
        }
        _ => None,
    };
    let named = match &ev.logical_key {
        Key::Named(NamedKey::ArrowLeft) => Some(TNamed::ArrowLeft),
        Key::Named(NamedKey::ArrowRight) => Some(TNamed::ArrowRight),
        Key::Named(NamedKey::Home) => Some(TNamed::Home),
        Key::Named(NamedKey::End) => Some(TNamed::End),
        Key::Named(NamedKey::Backspace) => Some(TNamed::Backspace),
        Key::Named(NamedKey::Delete) => Some(TNamed::Delete),
        _ => None,
    };
    field_edit_for(FieldStroke {
        ctrl: mods.control_key(),
        alt: mods.alt_key(),
        sup: mods.super_key(),
        named,
        base,
        text: ev.text.as_deref(),
    })
}

/// [`field_edit_action`] for an engine key.
pub(crate) fn field_edit_action_engine(
    key: &aterm_types::keyboard::Key,
    mods: aterm_types::keyboard::Modifiers,
    layout_text: Option<char>,
) -> Option<crate::app_search::SearchEdit> {
    use aterm_types::keyboard::{Key as TKey, Modifiers, NamedKey as TNamed};
    let (named, base, typed) = match key {
        TKey::Named(named) => (Some(*named), None, (*named == TNamed::Space).then_some(' ')),
        TKey::Character(c) => (
            None,
            Some(*c),
            Some(crate::input::typed_glyph(*c, mods, layout_text)),
        ),
        _ => (None, None, None),
    };
    let typed = typed.map(String::from);
    field_edit_for(FieldStroke {
        ctrl: mods.contains(Modifiers::CTRL),
        alt: mods.contains(Modifiers::ALT),
        sup: mods.contains(Modifiers::SUPER),
        named,
        base,
        text: typed.as_deref(),
    })
}

/// One keystroke as the field keymap reads it.
struct FieldStroke<'a> {
    ctrl: bool,
    alt: bool,
    sup: bool,
    named: Option<aterm_types::keyboard::NamedKey>,
    base: Option<char>,
    text: Option<&'a str>,
}

/// The field keymap ([`field_edit_action`]).
fn field_edit_for(stroke: FieldStroke<'_>) -> Option<crate::app_search::SearchEdit> {
    use crate::app_search::SearchEdit;
    use aterm_types::keyboard::NamedKey as TNamed;
    let FieldStroke {
        ctrl: ctrl_down,
        alt,
        sup,
        named,
        base,
        text,
    } = stroke;
    let base_is = |want: char| base.is_some_and(|c| c.eq_ignore_ascii_case(&want));
    let ctrl = ctrl_down && !sup && !alt;
    let by_word = alt && !ctrl_down && !sup;
    #[cfg(not(target_os = "macos"))]
    let by_word = by_word || ctrl;
    let to_edge = sup && !ctrl_down && !alt;
    match named {
        Some(TNamed::ArrowLeft) if to_edge => Some(SearchEdit::MoveStart),
        Some(TNamed::ArrowRight) if to_edge => Some(SearchEdit::MoveEnd),
        Some(TNamed::ArrowLeft) if by_word => Some(SearchEdit::MoveWordLeft),
        Some(TNamed::ArrowRight) if by_word => Some(SearchEdit::MoveWordRight),
        Some(TNamed::ArrowLeft) => Some(SearchEdit::MoveCharLeft),
        Some(TNamed::ArrowRight) => Some(SearchEdit::MoveCharRight),
        Some(TNamed::Home) => Some(SearchEdit::MoveStart),
        Some(TNamed::End) => Some(SearchEdit::MoveEnd),
        Some(TNamed::Backspace) if to_edge => Some(SearchEdit::KillToStart),
        Some(TNamed::Backspace) if by_word => Some(SearchEdit::DeleteWordBack),
        Some(TNamed::Backspace) => Some(SearchEdit::DeleteBack),
        Some(TNamed::Delete) if by_word => Some(SearchEdit::DeleteWordForward),
        Some(TNamed::Delete) => Some(SearchEdit::DeleteForward),
        _ if ctrl && base_is('a') => Some(SearchEdit::MoveStart),
        _ if ctrl && base_is('e') => Some(SearchEdit::MoveEnd),
        _ if ctrl && base_is('b') => Some(SearchEdit::MoveCharLeft),
        _ if ctrl && base_is('f') => Some(SearchEdit::MoveCharRight),
        _ if ctrl && base_is('d') => Some(SearchEdit::DeleteForward),
        _ if ctrl && base_is('h') => Some(SearchEdit::DeleteBack),
        _ if ctrl && base_is('k') => Some(SearchEdit::KillToEnd),
        _ if ctrl && base_is('u') => Some(SearchEdit::KillToStart),
        _ if ctrl && base_is('w') => Some(SearchEdit::DeleteWordBack),
        _ if !sup && !ctrl_down => text
            .map(|text| -> String { text.chars().filter(|c| !c.is_control()).collect() })
            .filter(|text| !text.is_empty())
            .map(SearchEdit::Insert),
        _ => None,
    }
}

/// A ⌘ (Super) chord on a character key.
const fn overlay_command_chord(mods: ModifiersState, ev: &KeyEvent) -> bool {
    mods.super_key() && matches!(ev.logical_key, Key::Character(_))
}

/// Whether this is a bare modifier press.
const fn is_bare_modifier_key(key: &Key) -> bool {
    matches!(
        key,
        Key::Named(
            NamedKey::Shift
                | NamedKey::Control
                | NamedKey::Alt
                | NamedKey::Super
                | NamedKey::Meta
                | NamedKey::Hyper
                | NamedKey::Symbol
                | NamedKey::AltGraph
                | NamedKey::CapsLock
                | NamedKey::NumLock
                | NamedKey::ScrollLock
                | NamedKey::Fn
                | NamedKey::FnLock
        )
    )
}

/// Whether the hardcoded Cmd (Super) shortcut suite is live.
pub(crate) const HARDCODED_SUPER_CHORDS: bool = !cfg!(target_os = "linux");

/// The default scrollback chord, if any.
const fn scrollback_chord(mods: ModifiersState, ev: &KeyEvent) -> Option<ScrollIntent> {
    // ⌘↓ jumps to the live bottom and ⌘↑ to the oldest retained line, a way back to
    // live without typing. Routed as a `ScrollIntent` like the other scrollback
    // chords, so the selection survives.
    if HARDCODED_SUPER_CHORDS
        && mods.super_key()
        && !mods.shift_key()
        && !mods.control_key()
        && !mods.alt_key()
    {
        return match ev.logical_key {
            Key::Named(NamedKey::ArrowDown) => Some(ScrollIntent::Bottom),
            Key::Named(NamedKey::ArrowUp) => Some(ScrollIntent::Top),
            _ => None,
        };
    }
    let windows_terminal_chord = cfg!(windows)
        && mods.shift_key()
        && mods.control_key()
        && !mods.alt_key()
        && !mods.super_key();
    if !windows_terminal_chord
        && (!mods.shift_key() || mods.control_key() || mods.alt_key() || mods.super_key())
    {
        return None;
    }
    let jumps = windows_terminal_chord || !cfg!(windows);
    match ev.logical_key {
        Key::Named(NamedKey::PageUp) => Some(ScrollIntent::Up),
        Key::Named(NamedKey::PageDown) => Some(ScrollIntent::Down),
        Key::Named(NamedKey::Home) if jumps => Some(ScrollIntent::Top),
        Key::Named(NamedKey::End) if jumps => Some(ScrollIntent::Bottom),
        _ => None,
    }
}

/// Whether this press is the OS "show the context menu" chord aterm may claim.
#[cfg(any(windows, target_os = "linux"))]
fn tab_menu_chord(
    policy: crate::app_config::TabMenuChord,
    mods: aterm_types::keyboard::Modifiers,
    key: &aterm_types::keyboard::Key,
) -> bool {
    use aterm_types::keyboard::{Key as EKey, Modifiers, NamedKey as ENamed};
    match key {
        EKey::Named(ENamed::ContextMenu) => policy.claims_menu_key(),
        EKey::Named(ENamed::F10) => {
            policy.claims_shift_f10()
                && mods.contains(Modifiers::SHIFT)
                && !mods.intersects(Modifiers::CTRL | Modifiers::ALT | Modifiers::SUPER)
        }
        _ => false,
    }
}

/// Whether an open find field suspends a configured `[keybindings]` action.
const fn find_suspends_action(action: keybinding::Action) -> bool {
    use keybinding::Action as A;
    matches!(
        action,
        A::Copy
            | A::SelectAll
            | A::Paste
            | A::PasteSelection
            | A::ScrollPageUp
            | A::ScrollPageDown
            | A::ScrollLineUp
            | A::ScrollLineDown
            | A::ScrollToTop
            | A::ScrollToBottom
            | A::JumpPrevPrompt
            | A::JumpNextPrompt
            | A::FocusPaneLeft
            | A::FocusPaneRight
    )
}

/// Classify the terminal-only Emacs navigation chords after layout normalization.
fn terminal_emacs_search_direction(
    base: &Key,
    mods: ModifiersState,
    agent_composer: impl FnOnce() -> bool,
) -> Option<bool> {
    let bare_super = HARDCODED_SUPER_CHORDS && mods == ModifiersState::SUPER;
    let bare_ctrl = mods == ModifiersState::CONTROL;
    if !bare_super && !bare_ctrl {
        return None;
    }
    let forward = match base {
        Key::Character(key) if key.eq_ignore_ascii_case("s") => true,
        Key::Character(key) if key.eq_ignore_ascii_case("r") => false,
        _ => return None,
    };
    (bare_super || agent_composer()).then_some(forward)
}

/// The shipping `SessionFactory::handed_terminal`.
pub(crate) fn agent_handed_terminal(pgid: i32) -> bool {
    let Some(leader) = u32::try_from(pgid).ok().filter(|pgid| *pgid > 0) else {
        return false;
    };
    aterm_sysprobe::stdin_sharers(leader).is_some_and(|members| {
        members.into_iter().any(|member| {
            i32::try_from(member)
                .ok()
                .and_then(crate::session_status::program::program_of)
                .is_some_and(|name| hands_the_terminal_off(&name))
        })
    })
}

/// Whether a member named `program` that reads its group leader's input took the terminal from it.
fn hands_the_terminal_off(program: &str) -> bool {
    aterm_phase::program_of(program).is_none() && !aterm_phase::may_host_agent(program)
}

fn font_zoom_repeat_action(
    mods: ModifiersState,
    ev: &KeyEvent,
) -> Option<crate::FontZoomRepeatAction> {
    // Part of the hardcoded Cmd suite: off Linux only (the seeded `ctrl+=` /
    // `ctrl+-` / `ctrl+0` keybindings are the zoom chords there).
    if !HARDCODED_SUPER_CHORDS || !mods.super_key() {
        return None;
    }
    let Key::Character(character) = &ev.logical_key else {
        return None;
    };
    match character.as_str() {
        "=" | "+" => Some(crate::FontZoomRepeatAction::Increase),
        "-" => Some(crate::FontZoomRepeatAction::Decrease),
        "0" => Some(crate::FontZoomRepeatAction::Reset),
        _ => None,
    }
}

/// A bare submitted-turn boundary.
fn is_plain_enter(ev: &InputEvent) -> bool {
    use aterm_types::keyboard::{Key as TKey, Modifiers as TMods, NamedKey as TNamed};

    let folded = ev.keypad_folded();
    matches!(
        folded.as_ref().unwrap_or(ev),
        InputEvent::Key {
            key: TKey::Named(TNamed::Enter),
            mods,
            ..
        } if !mods.intersects(TMods::SHIFT | TMods::CTRL | TMods::ALT | TMods::SUPER)
    )
}

thread_local! {
    static RELEASE_RELEVANCE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Publishes the press-time release relevance of `session`.
fn publish_release_relevance(session: u64, relevant: bool) {
    let packed = (session.wrapping_add(1) << 1) | u64::from(relevant);
    RELEASE_RELEVANCE.with(|slot| slot.set(packed));
}

/// `Some(false)` iff the last press-path sample for `session` proved a key release encodes nothing.
fn sampled_release_relevance(session: u64) -> Option<bool> {
    let packed = RELEASE_RELEVANCE.with(std::cell::Cell::get);
    if packed == 0 || packed >> 1 != session.wrapping_add(1) {
        return None;
    }
    Some(packed & 1 == 1)
}

/// Mints a fresh App-wide kitty-summon ident.
const fn next_kitty_summon_ident(seq: &mut u64, tag: u64) -> u64 {
    *seq = seq.wrapping_add(1);
    tag ^ *seq
}

/// What a press hands the predictor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PredictKey {
    Glyph(char),
    Edit(crate::predict::EditKey),
}

/// One pure classification of a committed Key/Text/KeySequence press.
struct PressClass<'ev> {
    console_input: Option<PetInputKind>,
    predict_candidate: Option<PredictKey>,
    typed_forward: Option<bool>,
    navigation_key: bool,
    kill_key: bool,
    kill_moves: bool,
    kill_word: bool,
    line_abort: bool,
    job_chord: bool,
    redraw_chord: bool,
    enter_like: bool,
    keyed_enter: bool,
    line_terminator: bool,
    tab_key: bool,
    insert_tab: bool,
    paste_chord_key: bool,
    typed: Option<char>,
    ime: Option<&'ev str>,
    backspace: bool,
    brk: bool,
    inert_modifier: bool,
    spacebar: bool,
    shift_key: bool,
    glyph_shifted: bool,
}

/// Whether committed text can move the terminal cursor.
fn committed_text_moves_cursor(text: &str) -> bool {
    text.chars().any(|ch| matches!(ch, '\t' | '\n' | '\r')) || committed_text_cells(text, false) > 0
}

/// Exact terminal-cell advance of one committed Text/IME event.
fn committed_text_cells(text: &str, ambiguous_width_double: bool) -> u16 {
    // A base-less run of combining marks, selectors or joiners does not advance the
    // terminal, even if the classifier groups it into an emoji-shaped cluster.
    if aterm_grapheme::str_width(text) == 0 {
        return 0;
    }
    u16::try_from(aterm_grapheme::str_grid_columns_with(
        text,
        ambiguous_width(ambiguous_width_double),
    ))
    .unwrap_or(u16::MAX)
}

/// The terminal's ambiguous-width setting (single, or the CJK double).
const fn ambiguous_width(
    ambiguous_width_double: bool,
) -> aterm_types::text_shaping::AmbiguousWidth {
    use aterm_types::text_shaping::AmbiguousWidth;

    if ambiguous_width_double {
        AmbiguousWidth::Double
    } else {
        AmbiguousWidth::Single
    }
}

/// The columns each grapheme of `text` advances the cursor, in order.
fn grapheme_advances(text: &str, ambiguous_width_double: bool) -> impl Iterator<Item = u16> + '_ {
    let mut end = 0;
    aterm_grapheme::grid_graphemes_with(text, ambiguous_width(ambiguous_width_double)).map(
        move |g| {
            let advance = g.end_col.saturating_sub(end);
            end = g.end_col;
            u16::try_from(advance).unwrap_or(u16::MAX)
        },
    )
}

/// The per-grapheme cell widths of one committed Text/IME event, appended to `out` in order.
#[cfg(test)]
fn committed_grapheme_cells_into(text: &str, ambiguous_width_double: bool, out: &mut Vec<u16>) {
    if aterm_grapheme::str_width(text) == 0 {
        return;
    }
    out.extend(grapheme_advances(text, ambiguous_width_double));
}

/// The Backspace price memory.
#[derive(Default)]
pub(crate) struct ErasePriceMemory(Vec<u16>);

impl ErasePriceMemory {
    /// How many entries the memory keeps.
    pub(crate) const CAP: usize = crate::cursor_glow::TYPED_STAMP_DEPTH;

    /// One typed glyph's width, behind everything already priced.
    fn push(&mut self, width: u16) {
        self.0.push(width);
        self.trim();
    }

    /// A committed Text/IME run ([`committed_text_cells`]).
    fn price_graphemes(&mut self, text: &str, ambiguous_width_double: bool) {
        if aterm_grapheme::str_width(text) == 0 {
            return;
        }
        let mut widths = grapheme_advances(text, ambiguous_width_double).peekable();
        while widths.peek().is_some() {
            self.0.extend(widths.by_ref().take(Self::CAP));
            self.trim();
        }
    }

    /// `n` priced zeros.
    fn push_zeros(&mut self, n: usize) {
        self.0.extend(std::iter::repeat_n(0u16, n));
        self.trim();
    }

    /// The price of the glyph the newest Backspace erases.
    fn pop(&mut self) -> Option<u16> {
        self.0.pop()
    }

    /// Close the memory.
    fn clear(&mut self) {
        self.0.clear();
    }

    /// The bound: newest kept (a Backspace prices from the back).
    fn trim(&mut self) {
        let excess = self.0.len().saturating_sub(Self::CAP);
        if excess > 0 {
            self.0.drain(..excess);
        }
    }

    #[cfg(test)]
    fn widths(&self) -> &[u16] {
        &self.0
    }
}

#[cfg(test)]
mod erase_price_memory_tests {
    use super::{ErasePriceMemory, committed_grapheme_cells_into};

    #[test]
    fn large_commits_keep_only_the_bounded_erase_tail_without_retaining_their_allocation() {
        let long = "x日e\u{301}👩🏽‍💻·".repeat(4096);
        for double_width in [false, true] {
            let mut memory = ErasePriceMemory::default();
            let mut reference = Vec::new();
            memory.push(1);
            memory.push_zeros(ErasePriceMemory::CAP);
            reference.resize(ErasePriceMemory::CAP, 0);
            for text in ["ab", "\u{301}", "\u{301}日本", long.as_str(), "⚠️z"] {
                committed_grapheme_cells_into(text, double_width, &mut reference);
                if text == long {
                    assert!(reference.capacity() > 2 * ErasePriceMemory::CAP);
                }
                let excess = reference.len().saturating_sub(ErasePriceMemory::CAP);
                reference.drain(..excess);
                memory.price_graphemes(text, double_width);
                assert_eq!(memory.widths(), reference, "{double_width}");
                assert!(memory.0.capacity() < 4 * ErasePriceMemory::CAP);
                assert_eq!(memory.pop(), reference.pop());
            }
            memory.clear();
            assert!(memory.widths().is_empty());
            assert!(memory.0.capacity() < 4 * ErasePriceMemory::CAP);
            memory.price_graphemes("é", double_width);
            assert_eq!(memory.pop(), Some(if double_width { 2 } else { 1 }));
            assert_eq!(memory.pop(), None);
        }
    }
}

fn committed_char_cells(ch: char, ambiguous_width_double: bool) -> u16 {
    let width = if ambiguous_width_double {
        aterm_grapheme::char_width_cjk(ch)
    } else {
        aterm_grapheme::char_width(ch)
    };
    u16::try_from(width).unwrap_or(u16::MAX)
}

fn classify_press(ev: &InputEvent) -> PressClass<'_> {
    use aterm_types::keyboard::{Key as TKey, Modifiers as TMods, NamedKey as TNamed};
    let ime: Option<&str> = match ev {
        InputEvent::Text(t) if !t.is_empty() => Some(t),
        _ => None,
    };
    let bare = |mods: &TMods| {
        mods.difference(TMods::CAPS_LOCK | TMods::NUM_LOCK)
            .is_empty()
    };
    let ctrl_only =
        |mods: &TMods| mods.difference(TMods::CAPS_LOCK | TMods::NUM_LOCK) == TMods::CTRL;
    let edit_arrow = match ev {
        InputEvent::Key {
            key: TKey::Named(TNamed::ArrowLeft | TNamed::NumpadArrowLeft),
            mods,
            ..
        } if bare(mods) => Some(crate::predict::EditKey::Left),
        InputEvent::Key {
            key: TKey::Named(TNamed::ArrowRight | TNamed::NumpadArrowRight),
            mods,
            ..
        } if bare(mods) => Some(crate::predict::EditKey::Right),
        InputEvent::Key {
            key: TKey::Named(TNamed::Home),
            mods,
            ..
        } if bare(mods) => Some(crate::predict::EditKey::Home),
        InputEvent::Key {
            key: TKey::Named(TNamed::End),
            mods,
            ..
        } if bare(mods) => Some(crate::predict::EditKey::End),
        InputEvent::Key {
            key: TKey::Named(TNamed::Delete),
            mods,
            ..
        } if bare(mods) => Some(crate::predict::EditKey::Delete),
        _ => None,
    };
    let edit_chord = match ev {
        InputEvent::Key {
            key: TKey::Character(c),
            mods,
            ..
        } if ctrl_only(mods) && c.eq_ignore_ascii_case(&'a') => {
            Some(crate::predict::EditKey::CtrlA)
        }
        InputEvent::Key {
            key: TKey::Character(c),
            mods,
            ..
        } if ctrl_only(mods) && c.eq_ignore_ascii_case(&'e') => {
            Some(crate::predict::EditKey::CtrlE)
        }
        _ => None,
    };
    let folded = ev.keypad_folded();
    let ev = folded.as_ref().unwrap_or(ev);
    let predict_candidate: Option<PredictKey> = match ev {
        InputEvent::Key {
            key,
            mods,
            layout_text,
            ..
        } if !mods.contains(TMods::CTRL)
            && !mods.contains(TMods::ALT)
            && !mods.contains(TMods::SUPER) =>
        {
            match key {
                TKey::Character(c) => Some(PredictKey::Glyph(input::typed_glyph(
                    *c,
                    *mods,
                    *layout_text,
                ))),
                TKey::Named(TNamed::Space) => Some(PredictKey::Glyph(' ')),
                TKey::Named(TNamed::Backspace) => {
                    Some(PredictKey::Edit(crate::predict::EditKey::Backspace))
                }
                _ => edit_arrow.map(PredictKey::Edit),
            }
        }
        InputEvent::Text(s) => {
            let mut chars = s.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if !c.is_control() && aterm_grapheme::char_width(c) == 1 => {
                    Some(PredictKey::Glyph(c))
                }
                _ => None,
            }
        }
        _ => None,
    }
    .or(edit_chord.map(PredictKey::Edit));
    let typed_forward: Option<bool> = match ev {
        InputEvent::Key { key, mods, .. }
            if !mods.contains(TMods::CTRL)
                && !mods.contains(TMods::ALT)
                && !mods.contains(TMods::SUPER) =>
        {
            match key {
                TKey::Character(c) if aterm_grapheme::char_width(*c) > 0 => Some(true),
                TKey::Named(TNamed::Space | TNamed::Enter) => Some(true),
                TKey::Named(TNamed::Backspace) => Some(false),
                _ => None,
            }
        }
        InputEvent::Text(t) if committed_text_moves_cursor(t) => Some(true),
        _ => None,
    };
    let navigation_key: bool = match ev {
        InputEvent::Key { key, mods, .. } if !mods.contains(TMods::SUPER) => {
            let ctrl = mods.contains(TMods::CTRL);
            let alt = mods.contains(TMods::ALT);
            match key {
                TKey::Named(
                    TNamed::ArrowLeft
                    | TNamed::ArrowRight
                    | TNamed::ArrowUp
                    | TNamed::ArrowDown
                    | TNamed::Home
                    | TNamed::End
                    | TNamed::PageUp
                    | TNamed::PageDown,
                ) => true,
                TKey::Character(c) if ctrl && matches!(c, 'a' | 'e' | 'b' | 'f' | 'p' | 'n') => {
                    true
                }
                TKey::Character(c) if alt && matches!(c, 'b' | 'f') => true,
                _ => false,
            }
        }
        _ => false,
    };
    let (kill_key, kill_moves, kill_word): (bool, bool, bool) = match ev {
        InputEvent::Key { key, mods, .. } => {
            let ctrl = mods.contains(TMods::CTRL);
            let alt = mods.contains(TMods::ALT);
            match key {
                TKey::Character(c) if ctrl && *c == 'k' => (true, false, false),
                TKey::Character(c) if ctrl && *c == 'u' => (true, true, false),
                TKey::Character(c) if ctrl && *c == 'w' => (true, true, true),
                TKey::Character(c) if alt && *c == 'd' => (true, false, true),
                TKey::Named(TNamed::Backspace) if ctrl || alt => (true, true, true),
                TKey::Named(TNamed::Delete) => (true, false, true),
                _ => (false, false, false),
            }
        }
        _ => (false, false, false),
    };
    let ctrl_chord = |wanted: &[char]| {
        matches!(
            ev,
            InputEvent::Key {
                key: TKey::Character(c),
                mods,
                ..
            } if wanted.contains(c)
                && mods.contains(TMods::CTRL)
                && !mods.contains(TMods::ALT)
                && !mods.contains(TMods::SUPER)
        )
    };
    let line_abort = ctrl_chord(&['c', 'z', '\\', 'j', 'm']);
    let job_chord = ctrl_chord(&['z', '\\']);
    let redraw_chord = ctrl_chord(&['l']);
    let enter_like = matches!(
        ev,
        InputEvent::Key {
            key: TKey::Named(TNamed::Enter),
            ..
        }
    );
    let tab_key = matches!(
        ev,
        InputEvent::Key {
            key: TKey::Named(TNamed::Tab),
            ..
        }
    );
    let insert_tab = matches!(
        ev,
        InputEvent::Key {
            key: TKey::Named(TNamed::Tab),
            mods,
            ..
        } if !mods.intersects(TMods::SHIFT | TMods::CTRL | TMods::ALT | TMods::SUPER)
    );
    let paste_chord_key = matches!(
        ev,
        InputEvent::Key {
            key: TKey::Character('v'),
            mods,
            ..
        } if mods.contains(TMods::CTRL)
            && !mods.contains(TMods::ALT)
            && !mods.contains(TMods::SUPER)
    );
    let keyed_enter = enter_like
        || matches!(
            ev,
            InputEvent::KeySequence(bytes) if bytes.last().is_some_and(|b| matches!(b, b'\r' | b'\n'))
        );
    let line_terminator = matches!(
        ev,
        InputEvent::Key {
            key: TKey::Named(TNamed::Enter),
            mods,
            ..
        } if !mods.intersects(TMods::ALT | TMods::SUPER)
    ) || ctrl_chord(&['j', 'm'])
        || matches!(ev, InputEvent::KeySequence(bytes) if aterm_predict::is_line_submit(bytes));
    let mut typed: Option<char> = None;
    let mut backspace = false;
    let mut brk = false;
    let mut glyph_shifted = false;
    match ev {
        InputEvent::Key {
            key,
            mods,
            layout_text,
            ..
        } => {
            let chorded = mods.contains(TMods::CTRL)
                || mods.contains(TMods::ALT)
                || mods.contains(TMods::SUPER);
            match key {
                TKey::Character(c) if !chorded => {
                    typed = Some(input::typed_glyph(*c, *mods, *layout_text));
                    glyph_shifted = mods.contains(TMods::SHIFT);
                }
                TKey::Named(TNamed::Space) if !chorded => typed = Some(' '),
                TKey::Named(TNamed::Backspace) if !chorded => backspace = true,
                TKey::Character(_) => brk = true,
                TKey::Named(
                    TNamed::Enter
                    | TNamed::Tab
                    | TNamed::Escape
                    | TNamed::Delete
                    | TNamed::Backspace
                    | TNamed::ArrowLeft
                    | TNamed::ArrowRight
                    | TNamed::ArrowUp
                    | TNamed::ArrowDown
                    | TNamed::Home
                    | TNamed::End
                    | TNamed::PageUp
                    | TNamed::PageDown,
                ) => brk = true,
                _ => {}
            }
        }
        InputEvent::Text(_) => {}
        InputEvent::KeySequence(_) => brk = true,
        _ => {}
    }
    let inert_modifier = matches!(
        ev,
        InputEvent::Key { key, .. } if aterm_types::keyboard::is_modifier_or_lock_key(key)
    );
    let spacebar = matches!(
        ev,
        InputEvent::Key {
            key: TKey::Named(TNamed::Space),
            mods,
            ..
        } if !mods.contains(TMods::CTRL)
            && !mods.contains(TMods::ALT)
            && !mods.contains(TMods::SUPER)
    );
    let shift_key = matches!(
        ev,
        InputEvent::Key {
            key: TKey::Named(TNamed::ShiftLeft | TNamed::ShiftRight),
            mods,
            ..
        } if !mods.contains(TMods::CTRL)
            && !mods.contains(TMods::ALT)
            && !mods.contains(TMods::SUPER)
    );
    let console_input = if matches!(
        ev,
        InputEvent::Key {
            event_type: aterm_types::keyboard::KeyEventType::Release,
            ..
        }
    ) || inert_modifier
    {
        None
    } else if enter_like {
        Some(PetInputKind::Submit)
    } else if backspace || kill_key {
        Some(PetInputKind::Delete)
    } else if navigation_key || tab_key {
        Some(PetInputKind::Navigate)
    } else if typed.is_some() || ime.is_some() {
        Some(PetInputKind::Text)
    } else {
        None
    };
    PressClass {
        console_input,
        predict_candidate,
        typed_forward,
        navigation_key,
        kill_key,
        kill_moves,
        kill_word,
        line_abort,
        job_chord,
        redraw_chord,
        enter_like,
        keyed_enter,
        line_terminator,
        tab_key,
        insert_tab,
        paste_chord_key,
        typed,
        ime,
        backspace,
        brk,
        inert_modifier,
        spacebar,
        shift_key,
        glyph_shifted,
    }
}

impl PressClass<'_> {
    /// A line-editing press.
    fn edits_the_line(&self) -> bool {
        self.typed_forward == Some(false) || self.navigation_key || self.kill_key
    }
}

/// The licence fence one input event draws before its dispatch ([`App::input`]).
pub(crate) fn press_fence(ev: &InputEvent) -> Option<bool> {
    match ev {
        InputEvent::Key {
            event_type: aterm_types::keyboard::KeyEventType::Release,
            ..
        }
        | InputEvent::MouseMove { buttons: 3, .. } => None,
        InputEvent::Paste(..) => Some(true),
        _ => {
            let class = classify_press(ev);
            let arms_own_license = (class.typed_forward == Some(true) && !class.enter_like)
                || is_plain_enter(ev)
                || class.tab_key
                || class.paste_chord_key
                || class.inert_modifier;
            if class.edits_the_line() {
                Some(true)
            } else if arms_own_license {
                None
            } else {
                Some(false)
            }
        }
    }
}

/// The engine-neutral event a physical press delivers to the seam under the window's modifier
/// snapshot `mods`.
fn physical_press(
    ev: &KeyEvent,
    mods: ModifiersState,
    option_composes: bool,
) -> Option<InputEvent> {
    if let Some((key, mods, base_layout, layout_text)) =
        keymap::build_key_input(ev, keymap::modifiers_from_winit(mods), option_composes)
    {
        return Some(InputEvent::Key {
            key,
            mods,
            base_layout,
            layout_text,
            event_type: aterm_types::keyboard::KeyEventType::Press,
        });
    }
    let bare = !mods.control_key() && !mods.alt_key() && !mods.super_key();
    ev.text
        .as_ref()
        .filter(|text| bare && !text.is_empty())
        .map(|text| InputEvent::Text(text.to_string()))
}

/// The hardcoded ⌘V (macOS and Windows, where the window owns Super).
fn hardcoded_paste_chord(mods: ModifiersState, ev: &KeyEvent) -> bool {
    HARDCODED_SUPER_CHORDS
        && mods.super_key()
        && matches!(&ev.logical_key, Key::Character(s) if s.eq_ignore_ascii_case("v"))
}

/// Whether a delivery is the first landing of a physical press or an auto-repeat tick.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PressPhase {
    /// The press as the user's finger first landed it (and every non-hold event).
    Initial,
    /// An auto-repeat tick of a hold whose press already took custody.
    Repeat,
}

/// The four-way press classification the custody record carries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PressKind {
    /// A byte-producing press landing for the first time.
    Typing,
    /// An auto-repeat tick of a hold whose press already took custody.
    Repeat,
    /// A bare modifier or lock key.
    Inert,
    /// A Kitty `REPORT_EVENT_TYPES` key-up.
    Release,
}

impl PressKind {
    /// Resolves the three overlapping press bits into one kind, in the priority on [`PressKind`].
    pub(crate) const fn of(is_release: bool, is_repeat: bool, inert_modifier: bool) -> Self {
        if is_release {
            Self::Release
        } else if is_repeat {
            Self::Repeat
        } else if inert_modifier {
            Self::Inert
        } else {
            Self::Typing
        }
    }

    /// Whether this press may disturb the user's reading position.
    pub(crate) const fn disturbs(self) -> bool {
        matches!(self, Self::Typing)
    }
}

/// [`note_person_reaching`] for a gesture that reached the program ([`note_person_event`]).
#[cfg(test)]
pub(crate) fn note_person(ctx: &crate::SessionCtx, gesture: crate::human_input::Gesture) -> bool {
    note_person_reaching(ctx, gesture, true)
}

/// A person's hands were on `ctx`'s session.
pub(crate) fn note_person_reaching(
    ctx: &crate::SessionCtx,
    gesture: crate::human_input::Gesture,
    reaches: bool,
) -> bool {
    let noted = ctx
        .human_input
        .note_reaching(crate::metrics::now_us(), gesture, reaches);
    deliver_noted(ctx, noted, reaches)
}

/// The timeline half of [`note_person_reaching`], for the gesture `noted` counted.
pub(crate) fn deliver_noted(
    ctx: &crate::SessionCtx,
    noted: crate::human_input::Noted,
    reaches: bool,
) -> bool {
    if noted.burst {
        let mut tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        let at = tl.slog().now();
        let own = crate::session_log::PersonGesture {
            burst: Some(noted.seq),
            reaches,
        };
        tl.slog_mut().person_input(Some(own), at);
        tl.ring_rows();
        tl.slog_take_wake()
    } else if let Ok(mut tl) = ctx.timeline.try_lock() {
        let at = tl.slog().now();
        let own = crate::session_log::PersonGesture {
            burst: None,
            reaches,
        };
        tl.slog_mut().person_input(Some(own), at);
        tl.ring_rows();
        false
    } else {
        false
    }
}

/// [`note_person_reaching`] for the person's gesture `ev` as the seam receives it.
pub(crate) fn note_person_event(ctx: &crate::SessionCtx, ev: &InputEvent) -> bool {
    note_person_reaching(
        ctx,
        crate::human_input::Gesture::of(ev),
        crate::human_input::reaches_program(ev, &ctx.modes, crate::input::WheelPlatform::Host),
    )
}

/// The press half of the custody record.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "PressCustody",
        action = "TypingPress",
        project = "aterm_gui::press_custody_conformance::project_press_custody"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "PressCustody",
        action = "RepeatPress",
        project = "aterm_gui::press_custody_conformance::project_press_custody"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "PressCustody",
        action = "InertPress",
        project = "aterm_gui::press_custody_conformance::project_press_custody"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "PressCustody",
        action = "ReleaseEvent",
        project = "aterm_gui::press_custody_conformance::project_press_custody"
    )
)]
pub(crate) fn note_press_custody(t: &mut Terminal, kind: PressKind) {
    t.note_custody(press_transition(kind));
}

/// The `PressCustody` transition a press class is.
const fn press_transition(kind: PressKind) -> CustodyTransition {
    match kind {
        PressKind::Typing => CustodyTransition::TypingPress,
        PressKind::Repeat => CustodyTransition::RepeatPress,
        PressKind::Inert => CustodyTransition::InertPress,
        PressKind::Release => CustodyTransition::ReleaseEvent,
    }
}

/// What a disturbing press does to the terminal's reading position.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SelectionCustody",
        action = "TypingPress",
        project = "aterm_gui::selection_custody_conformance::project_selection_custody"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SelectionCustody",
        action = "InertPress",
        project = "aterm_gui::selection_custody_conformance::project_selection_custody"
    )
)]
pub(crate) fn apply_press_custody(t: &mut Terminal, kind: PressKind) -> (bool, bool) {
    note_press_custody(t, kind);
    let disturbs = kind.disturbs();
    let scrolled = disturbs && t.grid().display_offset() != 0 && {
        t.scroll_to_bottom();
        true
    };
    if disturbs && !scrolled && t.grid().reader_aim_held() {
        t.scroll_to_bottom();
    }
    let cleared = disturbs && t.text_selection().has_selection() && {
        t.text_selection_mut().clear();
        true
    };
    if scrolled || cleared {
        t.note_custody_took(press_transition(kind));
    }
    (scrolled, cleared)
}

/// The bits of one classified press the kitty-command listener's cascade reads ([`PressClass`]).
#[derive(Clone, Copy)]
struct TrickPress<'ev> {
    tty_swallows: bool,
    enter_like: bool,
    keyed_enter: bool,
    line_abort: bool,
    job_chord: bool,
    redraw_chord: bool,
    kill_key: bool,
    kill_moves: bool,
    kill_word: bool,
    brk: bool,
    backspace: bool,
    typed: Option<char>,
    ime: Option<&'ev str>,
}

impl TrickPress<'_> {
    /// ⌃U, the one kill the listener reads as a line reset.
    const fn kills_line(self) -> bool {
        self.kill_key && self.kill_moves && !self.kill_word
    }

    /// The press ends the listener's line, judged or not.
    fn line_ended(self) -> bool {
        self.enter_like || self.keyed_enter || self.line_abort || self.kills_line()
    }
}

/// Feed one key press through the kitty-command listener cascade.
fn feed_trick_listener<'w>(
    listener: &'w mut aterm_effects::typed_tricks::TrickListener,
    tricks: &aterm_lexicon::TrickLexicon,
    session: u64,
    now: std::time::Instant,
    press: TrickPress<'_>,
) -> TrickFeed<'w> {
    listener.rekey(session);
    if press.tty_swallows {
        return if press.enter_like {
            listener.note_line_reset()
        } else {
            listener.note_no_echo()
        };
    }
    if press.enter_like {
        let lexicon = listener.submit_needs_lexicon().then_some(tricks);
        return listener.note_submit(now, lexicon);
    }
    if press.keyed_enter || (press.line_abort && !press.job_chord) || press.kills_line() {
        return listener.note_line_reset();
    }
    if press.job_chord {
        return listener.note_job_chord();
    }
    if press.kill_key {
        return if press.kill_word && press.kill_moves {
            listener.note_word_kill()
        } else {
            TrickFeed::NONE
        };
    }
    if press.redraw_chord {
        return TrickFeed::NONE;
    }
    if press.brk {
        return listener.note_break();
    }
    if press.backspace {
        return listener.note_backspace();
    }
    if let Some(c) = press.typed {
        let lexicon = listener.needs_lexicon(c).then_some(tricks);
        return listener.note_char(now, c, lexicon);
    }
    if let Some(text) = press.ime {
        let lexicon = listener.ime_needs_lexicon(text).then_some(tricks);
        return listener.note_ime(now, text, lexicon);
    }
    TrickFeed::NONE
}

/// What the host did with the last kitty-command fire on a window's current typed line.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum TrickLineFire {
    /// No fire on this line yet.
    #[default]
    None,
    /// The fire was admitted.
    Admitted { session: u64, addressed: bool },
    /// The fire was withheld.
    Withheld { session: u64 },
}

impl TrickLineFire {
    /// This memory as it stands for a press that went to `session`.
    const fn for_session(self, session: u64) -> Self {
        match self {
            Self::Admitted { session: on, .. } | Self::Withheld { session: on }
                if on != session =>
            {
                Self::None
            }
            same => same,
        }
    }
}

/// A tentative kitty command was taken back.
fn revoke_trick_line(ws: &mut crate::WindowState, now: std::time::Instant) {
    ws.fx.companion_mut().revoke_trick();
    if !matches!(
        ws.trick_fire,
        TrickLineFire::Admitted {
            addressed: true,
            ..
        }
    ) {
        ws.fx.decos_mut().revoke_trick_flash(now);
    }
    ws.trick_fire = TrickLineFire::None;
}

/// Revokes, on both engines, the cohort of one dispatch that did not reach the wire.
fn revoke_dispatch(ws: &mut crate::WindowState, at: std::time::Instant, write_failed: bool) {
    if write_failed {
        ws.fx.cursor_mut().glow.revoke_failed_input_at(at);
    } else {
        ws.fx.cursor_mut().glow.revoke_input_hints_at(at);
    }
    ws.fx.cursor_mut().trail.revoke_input_hints_at(at);
}

/// The failed edge, applied by the render prelude with the deliveries ([`Deliveries::failed`]).
pub(crate) fn revoke_failed_delivery(
    ws: &mut crate::WindowState,
    id: WindowId,
    ticket: DeliveryTicket,
) {
    if ticket.window == Some(id) {
        revoke_dispatch(ws, ticket.dispatched_at, true);
    }
}

/// A mouse, wheel or focus report refused because its session's input queue is full
/// ([`paste_order::report_ordered_or_inline`]).
fn note_refused_report(receipt: input::EgressReceipt) {
    if receipt.input_queue_full() {
        crate::metrics::note_input_refused();
    }
}

/// Write a UI-thread focus or colour-scheme report to a session outside [`App::input`].
pub(crate) fn write_ui_report(
    term: &Arc<Mutex<Terminal>>,
    modes: &Arc<ModeMirror>,
    sink: &Arc<SinkWriter>,
    ev: &InputEvent,
) {
    note_refused_report(paste_order::report_ordered_or_inline(term, modes, sink, ev));
}

/// Why a person's input did not reach the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputRefusal {
    /// A paste larger than the ordered queue holds even when it is empty.
    PasteTooLarge,
    /// Earlier input is still being written and the queue behind it is full.
    Busy,
    /// The session's input writer was gone.
    Unavailable,
}

impl InputRefusal {
    const fn of_rejected(rejected: paste_order::Rejected) -> Self {
        match rejected {
            paste_order::Rejected::TooLarge => Self::PasteTooLarge,
            paste_order::Rejected::Full => Self::Busy,
            paste_order::Rejected::Unavailable => Self::Unavailable,
        }
    }

    const fn line(self) -> &'static str {
        match self {
            Self::PasteTooLarge => "the paste is too large \u{2014} paste it in smaller pieces",
            Self::Busy => "earlier input is still being sent \u{2014} try again",
            Self::Unavailable => "nothing was sent \u{2014} try again",
        }
    }
}

impl App {
    /// Write one DEC 1004 focus report to `session` via [`paste_order::report_ordered_or_inline`].
    pub(super) fn write_focus_report(&mut self, session: u64, focused: bool) {
        let Some(live) = self.pool.get(session) else {
            return;
        };
        let arms = live.ctx.modes.focus_arms();
        write_ui_report(
            &live.term,
            &live.ctx.modes,
            &live.ctx.sink,
            &InputEvent::Focus(focused),
        );
        self.pool.set_focus_arms_told(session, arms);
    }

    /// A key or paste refused for `session`.
    fn refuse_input(&mut self, session: u64, why: InputRefusal) {
        crate::metrics::note_input_refused();
        let key = format!("session.input-full.{session}");
        if self
            .messages
            .live_by_key(&key)
            .is_some_and(|row| matches!(row.msg.detail.as_slice(), [line] if line == why.line()))
        {
            return;
        }
        self.post_message(crate::message_reporters::input_refused(why.line()).key(&key));
    }

    /// Closes every licence term at a newer input boundary.
    pub(crate) fn clear_move_license(&mut self, wid: WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.fx
                .cursor_mut()
                .glow
                .clear_typed(std::time::Instant::now());
            ws.fx.cursor_mut().trail.clear_typed();
        }
    }

    /// [`Self::clear_move_license`] for a boundary that cannot take the hand off its row.
    pub(crate) fn clear_move_license_keeping_hand(&mut self, wid: WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            let fx = ws.fx.cursor_mut();
            fx.glow.clear_typed_keeping_hand(std::time::Instant::now());
            fx.trail.clear_typed();
        }
    }

    /// The licence fence [`press_fence`] classified.
    pub(crate) fn fence_move_license(&mut self, wid: WindowId, keep_hand: bool) {
        if keep_hand {
            self.clear_move_license_keeping_hand(wid);
        } else {
            self.clear_move_license(wid);
        }
    }

    /// Whether a physical key press triggers a paste in this window.
    fn press_pastes(
        &self,
        mods: ModifiersState,
        ev: &KeyEvent,
        base_key: &mut Option<Key>,
    ) -> bool {
        hardcoded_paste_chord(mods, ev)
            || (!self.keybindings.is_empty()
                && self
                    .keybindings
                    .action_for(
                        base_key.get_or_insert_with(|| base_logical_key(ev)),
                        ev.physical_key,
                        mods,
                    )
                    .is_some_and(keybinding::Action::is_paste))
    }

    /// Applies the control socket's licence fences this window owes
    /// ([`crate::licence_fence::owed`]).
    pub(crate) fn reconcile_licence(&mut self, wid: WindowId) {
        let front = self.front_terminal(wid).map(|terminal| terminal.session);
        let pool = &self.pool;
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let owed = crate::licence_fence::owed(&mut ws.licence_seen, front, |session| {
            pool.get(session).map(|owner| owner.ctx.licence.clone())
        });
        if let Some(keep_hand) = owed {
            self.fence_move_license(wid, keep_hand);
        }
    }

    /// External OS command surfaces can consume input before `on_key`/`App::input`.
    pub(crate) fn clear_front_move_license(&mut self) {
        if let Some(wid) = self.frontmost_window {
            self.clear_move_license(wid);
        }
    }

    /// Licence-only ingress fence for native tab-strip wakes.
    pub(crate) fn clear_tab_surface_move_license(&mut self, window: Option<WindowId>) {
        match window {
            Some(wid) => self.clear_move_license(wid),
            None => self.clear_front_move_license(),
        }
    }

    /// The `App::input` convergence seam.
    #[track_caller]
    pub(crate) fn input(&mut self, wid: WindowId, ev: InputEvent, src: Source) -> InputOutcome {
        self.input_to_session(wid, ev, src, None, crate::app_input::PressPhase::Initial)
    }

    /// Whether tone-of-typing inference may run at all.
    fn tone_infer_active(&self) -> bool {
        self.trail_audio.is_live()
            && self.trail_audio.wedged_for().is_none()
            && self.config.tone_melody_or_default()
            && self.config.trail_sounds_or_default()
            && self.config.trail_sound_volume() > 0.0
    }

    /// The `tone` control-socket verb ([`crate::Wake::ToneStatus`]).
    pub(crate) fn tone_status(&mut self) -> Result<String, String> {
        let Some(wid) = self.frontmost_window else {
            return Err("no focused window".to_string());
        };
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.fx
                .companions_mut()
                .tone_tracker
                .drain_pending_inference(std::time::Instant::now());
        }
        let Some(ws) = self.windows.get(&wid) else {
            return Err("no focused window".to_string());
        };
        let knob = self.config.tone_melody_or_default();
        let host = self.trail_audio.host_state();
        let now = std::time::Instant::now();
        let focused = self.cursor_fx_focus(wid, ws.focused, now);
        let mode = self.config.motion_mode();
        let shed = self.effective_shed_envelope(mode, now);
        let glow = self.glow_config();
        let serious_sound = self
            .serious_mode_policy()
            .allows(crate::motion::SeriousEffect::TerminalSound);
        let inputs = crate::sound_seam::verb_seam_inputs(
            &glow,
            &ws.fx.cursor().glow,
            crate::sound_seam::HostSoundTerms {
                sounds_on: self.config.trail_sounds_or_default(),
                volume: self.config.trail_sound_volume(),
                serious_allows: serious_sound,
                focused,
                resize_quiet: ws.resize_sound_quiet(now),
                host,
            },
        );
        Ok(crate::tone_status::ToneStatus {
            tone: ws.fx.companions().tone_tracker.current(),
            effective: ws.fx.companions().tone_tracker.effective(knob),
            knob,
            sounds: self.config.trail_sounds_or_default(),
            volume: self.config.trail_sound_volume(),
            audio: host,
            active: self.tone_infer_active(),
            window_chars: ws.fx.companions().tone_tracker.window_chars(),
            inferences: ws.fx.companions().tone_tracker.inferences,
            dropped: self.trail_audio.dropped_cues(),
            seam: crate::sound_seam::sound_seam(&inputs),
            engine_sound: inputs.engine_sound_live,
            trail: inputs.trail_on,
            focused,
            serious_sound,
            motion_stage: match self.motion_policy(focused) {
                crate::motion::MotionPolicy::Full => "full",
                crate::motion::MotionPolicy::Reduced => "reduced",
            },
            shed,
            revives: self.trail_audio.revives_spent(),
            reopens_left: self.trail_audio.reopens_left(),
        }
        .line())
    }

    /// The `streak [status]` control verb ([`crate::Wake::StreakStatus`]).
    pub(crate) fn streak_status(&self) -> Result<String, String> {
        let Some(wid) = self.frontmost_window else {
            return Err("no focused window".to_string());
        };
        let Some(ws) = self.windows.get(&wid) else {
            return Err("no focused window".to_string());
        };
        let enabled = self.config.output_streak_enabled_or_default();
        let sound = self.config.output_streak_sound_or_default();
        let raw_focused = ws.focused;
        let fx_focused = self.cursor_fx_focus(wid, raw_focused, std::time::Instant::now());
        let animate = self
            .motion_policy(fx_focused)
            .animate(crate::motion::MotionEffect::OutputStreak);
        let panes = ws.fx.streak_panes().count();
        let engine = if ws.fx.streak().is_some() || panes > 0 {
            "live"
        } else {
            "none"
        };
        let active = ws.fx.streak().is_some_and(|s| s.is_active())
            || ws.fx.streak_panes().any(|s| s.is_active());
        Ok(format!(
            "config_enabled={enabled} sound_key={sound} motion_animates={animate} \
             focused={raw_focused} fx_focused={fx_focused} serious_sound={serious} \
             serious_visual={serious_visual} sounds_master={master} volume={volume} \
             engine={engine} panes={panes} \
             active={active} intensity={intensity} tail={tail} max_streaks={max} \
             idle_secs={idle}",
            serious = self
                .serious_mode_policy()
                .allows(crate::motion::SeriousEffect::TerminalSound),
            serious_visual = self
                .serious_mode_policy()
                .allows(crate::motion::SeriousEffect::OutputStreak),
            master = self.config.trail_sounds_or_default(),
            volume = self.config.trail_sound_volume(),
            intensity = self.config.output_streak_intensity_or_default(),
            tail = self.config.output_streak_tail_or_default(),
            max = self.config.output_streak_max_streaks_or_default(),
            idle = self.config.output_streak_idle_secs_or_default(),
        ))
    }

    /// Record a typed-"kitty" completion in the Kitty Log ([`aterm_effects::kitty_summon`]).
    fn record_typed_kitty(&mut self, wid: WindowId, session: u64, now: std::time::Instant) {
        use aterm_effects::kitty_registry::{
            KittyMagic, KittyShownAs, KittySighting, KittyType, TRAIT_BOW, TRAIT_CROWN,
        };
        let look = self.companion_verdict(wid, session, now).normalized();
        self.ensure_sparkle();
        let Some(rs) = self.sparkle.as_ref() else {
            return;
        };
        if !rs.cfg.feline {
            return;
        }
        let traits = match look.accessory {
            Some(aterm_effects::cat_glyphs_gen::CatGlyphId::AccBow) => TRAIT_BOW,
            Some(aterm_effects::cat_glyphs_gen::CatGlyphId::AccCrown) => TRAIT_CROWN,
            _ => 0,
        };
        let langs = rs
            .lexicon
            .scan("kitty", &aterm_lexicon::ScanOptions::default())
            .into_iter()
            .find(|m| matches!(m.class, aterm_lexicon::Class::Feline))
            .map_or(aterm_lexicon::LangSet::EMPTY, |m| m.langs);
        let ident = next_kitty_summon_ident(
            &mut self.kitty_summon_seq,
            aterm_effects::kitty_summon::TYPED_SUMMON_IDENT_TAG,
        );
        let sighting = KittySighting {
            kitty_type: KittyType::HeadPeek,
            magic: KittyMagic::None,
            shown_as: KittyShownAs::Cat,
            langs,
            traits,
            look,
            ident,
        };
        let enabled = self.kitty_log_enabled();
        let discovered = self
            .kitty_log
            .observe(session, [sighting], &rs.lexicon, now, enabled);
        let _ = discovered;
    }

    /// Promote this window's unpinned companion into the durable kitty registry and pin it.
    pub(crate) fn favourite_kitty_checked(
        &mut self,
        wid: WindowId,
        now: std::time::Instant,
    ) -> Result<(), String> {
        use aterm_effects::kitty_registry::{KittyMagic, KittyShownAs, KittySighting, KittyType};
        self.ensure_sparkle();
        let Some(rs) = self.sparkle.as_ref() else {
            return Err("effects are off, so there is no cursor cat to pin".to_string());
        };
        if !rs.cfg.feline {
            return Err("[sparkle_words.feline] is off, so cats do not exist".to_string());
        }
        let look = self.promotable_kitty(wid);
        let enabled = self.kitty_log_enabled();
        let ident = next_kitty_summon_ident(
            &mut self.kitty_summon_seq,
            aterm_effects::kitty_summon::FAVOURITE_IDENT_TAG,
        );
        let sighting = KittySighting {
            kitty_type: KittyType::HeadPeek,
            magic: KittyMagic::None,
            shown_as: KittyShownAs::Cat,
            langs: aterm_lexicon::LangSet::EMPTY,
            traits: 0,
            look,
            ident,
        };
        self.kitty_log
            .favourite(&sighting, &rs.lexicon, now, enabled);
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.fx.companions_mut().cursor_cat.on_collect(now, look);
            if let Some(w) = ws.os_window.as_ref() {
                w.request_redraw();
            }
        }
        Ok(())
    }

    /// Puts on a cat from the collection, by name, now.
    pub(crate) fn wear_kitty(
        &mut self,
        wid: WindowId,
        key: &str,
        now: std::time::Instant,
    ) -> Result<aterm_effects::kitty_registry::KittyLook, String> {
        self.ensure_sparkle();
        let Some(rs) = self.sparkle.as_ref() else {
            return Err("effects are off, so there is no cursor cat to dress".to_string());
        };
        if !rs.cfg.feline {
            return Err("[sparkle_words.feline] is off, so cats do not exist".to_string());
        }
        let enabled = self.kitty_log_enabled();
        let Some(look) = self.kitty_log.wear(key, now, enabled) else {
            return Err(format!("no cat named {key:?} in the collection"));
        };
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.fx.companions_mut().cursor_cat.on_collect(now, look);
            if let Some(w) = ws.os_window.as_ref() {
                w.request_redraw();
            }
        }
        Ok(look)
    }

    /// Wears the next cat in the collection.
    pub(crate) fn wear_next_kitty(
        &mut self,
        wid: WindowId,
        now: std::time::Instant,
    ) -> Result<aterm_effects::kitty_registry::KittyLook, String> {
        let (worn, rows) = self.kitty_log.wearable();
        if rows.is_empty() {
            return Err("no cats collected yet".to_string());
        }
        let at = worn
            .as_ref()
            .and_then(|key| rows.iter().position(|row| &row.key == key));
        let next = at.map_or(0, |i| (i + 1) % rows.len());
        let key = rows[next].key.clone();
        self.wear_kitty(wid, &key, now)
    }

    /// `kitty`: the wearable collection.
    pub(crate) fn kitty_collection_rows(&mut self) -> Result<Vec<String>, String> {
        let (worn, rows) = self.kitty_log.wearable();
        Ok(rows
            .iter()
            .map(|row| {
                let worn = u8::from(Some(&row.key) == worn.as_ref());
                format!(
                    "cat key={} coat={} iris={} age={} seen={} worn={worn}",
                    row.key, row.coat, row.iris, row.age, row.count
                )
            })
            .collect())
    }

    /// `kitty wear <key>`.
    pub(crate) fn wear_kitty_on_front(&mut self, key: &str) -> Result<String, String> {
        let Some(wid) = self.frontmost_window else {
            return Err("no window to dress".to_string());
        };
        let look = self.wear_kitty(wid, key, std::time::Instant::now())?;
        Ok(format!(
            "worn key={key} coat={} iris={}",
            look.coat, look.iris
        ))
    }

    /// The cat "Favourite This Kitty" promotes and its checkmark reports on.
    pub(crate) fn promotable_kitty(
        &self,
        wid: WindowId,
    ) -> aterm_effects::kitty_registry::KittyLook {
        self.windows
            .get(&wid)
            .and_then(|ws| ws.fx.look().tenure().worn().map(|i| i.look))
            .unwrap_or(self.launch_kitty)
    }

    /// Delivers a typed-word reaction to the window's cursor companion.
    fn react_typed_word(
        &mut self,
        wid: WindowId,
        now: std::time::Instant,
        hit: aterm_effects::kitty_summon::TypedHit,
    ) {
        self.ensure_sparkle();
        let Some(rs) = self.sparkle.as_ref() else {
            return;
        };
        if !rs.cfg.feline {
            return;
        }
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if hit.profanity
            && ws.fx.companions_mut().cursor_cat.on_curse(now, 1)
            && let Some(w) = ws.os_window.as_ref()
        {
            w.request_redraw();
        }
    }

    /// Summon the typed-word dog cameo ([`aterm_effects::kitty_summon`]).
    fn summon_typed_dog(&mut self, wid: WindowId, session: u64, now: std::time::Instant) {
        self.ensure_sparkle();
        let Some(rs) = self.sparkle.as_ref() else {
            return;
        };
        if !rs.cfg.canine {
            return;
        }
        let seed = next_kitty_summon_ident(
            &mut self.kitty_summon_seq,
            aterm_effects::kitty_summon::DOG_SUMMON_IDENT_TAG,
        );
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        ws.fx.companions_mut().dog_cameo.on_summon(now, seed);
        ws.fx.companions_mut().dog_cameo_session = Some(session);
        if let Some(w) = ws.os_window.as_ref() {
            w.request_redraw();
        }
    }

    /// Delivers a held-key payload to a session no longer frontmost in any window.
    fn input_to_hidden_session(
        &mut self,
        target_session: u64,
        ev: InputEvent,
        phase: PressPhase,
        completion: Option<crate::control_input_ack::InputCompletion>,
    ) -> InputOutcome {
        debug_assert!(matches!(
            ev,
            InputEvent::Key { .. }
                | InputEvent::Text(_)
                | InputEvent::KeySequence(_)
                | InputEvent::Paste(..)
        ));
        let Some(session) = self.pool.get(target_session) else {
            return InputOutcome::Ok;
        };
        let completion = completion.filter(|c| c.matches(target_session, &session.ctx));
        let (term, modes, sink, echo, ui_waiting) = (
            session.term.clone(),
            session.ctx.modes.clone(),
            session.ctx.sink.clone(),
            session.ctx.output_echo.clone(),
            session.ctx.ui_waiting.clone(),
        );
        let PressClass { inert_modifier, .. } = classify_press(&ev);
        let is_release = matches!(
            &ev,
            InputEvent::Key { event_type, .. }
                if matches!(event_type, aterm_types::keyboard::KeyEventType::Release)
        );
        let is_repeat = phase == PressPhase::Repeat
            || matches!(
                &ev,
                InputEvent::Key { event_type, .. }
                    if matches!(event_type, aterm_types::keyboard::KeyEventType::Repeat)
            );
        let press_kind = PressKind::of(is_release, is_repeat, inert_modifier);
        let site = crate::metrics::TermWaitSite::Press;
        if let InputEvent::Paste(text, _) = &ev {
            let bytes = text.len() as u64;
            let meter = aterm_messages::waits::paste_is_watched(bytes)
                .then(|| Arc::new(aterm_session::sink::BulkMeter::new()));
            return match paste_order::enqueue_metered_observed(
                &term,
                &modes,
                &sink,
                &echo,
                ev,
                None,
                None,
                meter.clone(),
                completion,
            ) {
                Ok(()) => {
                    let mut terminal = crate::term_lock_ui(&term, &ui_waiting, site);
                    let _ = apply_press_custody(&mut terminal, press_kind);
                    drop(terminal);
                    if let Some(meter) = meter {
                        self.watch_paste(
                            target_session,
                            &sink,
                            meter,
                            bytes,
                            std::time::Instant::now(),
                        );
                    }
                    InputOutcome::Ok
                }
                Err(rejected) => {
                    self.refuse_input(target_session, InputRefusal::of_rejected(rejected));
                    rejected_outcome(rejected)
                }
            };
        }
        {
            let mut terminal = crate::term_lock_ui(&term, &ui_waiting, site);
            let _ = apply_press_custody(&mut terminal, press_kind);
        }
        let (receipt, _) = paste_order::ordered_or_inline_observed(
            &term, &modes, &sink, &echo, &ev, None, None, completion,
        );
        if receipt.input_queue_full() {
            self.refuse_input(target_session, InputRefusal::Busy);
        }
        receipt_outcome(receipt)
    }

    /// Stamps `session`'s person clock ([`crate::SessionCtx::human_input`]) when `ev` is a
    /// person's gesture ([`crate::human_input::is_person_gesture`] from [`Source::Human`]).
    pub(crate) fn note_person_input(&self, session: u64, ev: &InputEvent, src: Source) {
        if matches!(src, Source::Human) && crate::human_input::is_person_gesture(ev) {
            self.spares.note_person(std::time::Instant::now());
            if let Some(owner) = self.pool.get(session) {
                let _ = note_person_event(&owner.ctx, ev);
            }
        }
    }

    /// Routes an input event through the convergence seam.
    #[track_caller]
    pub(crate) fn input_to_session(
        &mut self,
        wid: WindowId,
        ev: InputEvent,
        src: Source,
        target_session: Option<u64>,
        phase: PressPhase,
    ) -> InputOutcome {
        self.input_to_session_observed(wid, ev, src, target_session, phase, None)
    }

    #[track_caller]
    pub(crate) fn input_to_session_observed(
        &mut self,
        wid: WindowId,
        ev: InputEvent,
        src: Source,
        target_session: Option<u64>,
        phase: PressPhase,
        completion: Option<crate::control_input_ack::InputCompletion>,
    ) -> InputOutcome {
        let _site = crate::resize_ledger::SiteScope::enter(crate::resize_ledger::Cause::Input);
        self.input_to_session_inner(wid, ev, src, target_session, phase, completion)
    }

    /// The body of [`Self::input_to_session`].
    fn input_to_session_inner(
        &mut self,
        wid: WindowId,
        ev: InputEvent,
        src: Source,
        target_session: Option<u64>,
        phase: PressPhase,
        completion: Option<crate::control_input_ack::InputCompletion>,
    ) -> InputOutcome {
        self.ensure_sparkle();
        match crate::input_event_handoff_class(&ev) {
            crate::UpdateHandoffEventClass::Exempt => {}
            crate::UpdateHandoffEventClass::Tolerated => {
                self.note_update_handoff_tolerated_activity();
            }
            crate::UpdateHandoffEventClass::Revoking => self.note_update_handoff_activity(),
        }
        let _audit = src;
        if is_present_recovery_stimulus(&ev)
            && let Some(ws) = self.windows.get_mut(&wid)
        {
            ws.rearm_present(false);
        }
        if let InputEvent::Key {
            key, event_type, ..
        } = &ev
            && matches!(event_type, aterm_types::keyboard::KeyEventType::Release)
            && self
                .windows
                .get_mut(&wid)
                .is_some_and(|ws| ws.overlay_consumed_keys.remove(key))
        {
            return InputOutcome::Ok;
        }
        if let Some(keep_hand) = press_fence(&ev) {
            self.fence_move_license(wid, keep_hand);
        }
        if target_session.is_none()
            && self.windows.get(&wid).is_some_and(|ws| ws.overlay_open())
            && matches!(
                ev,
                InputEvent::Key { .. }
                    | InputEvent::KeySequence(_)
                    | InputEvent::Text(_)
                    | InputEvent::MouseButton { .. }
                    | InputEvent::MouseMove { .. }
                    | InputEvent::Wheel { .. }
                    | InputEvent::Paste(..)
                    | InputEvent::ScrollView(_)
            )
        {
            let untracked_release = matches!(
                &ev,
                InputEvent::Key { event_type, .. }
                    if matches!(event_type, aterm_types::keyboard::KeyEventType::Release)
            );
            if !untracked_release {
                // Record a press the overlay consumes so its release (possibly after the
                // overlay closes) is swallowed above. Presses only: a repeat of a
                // pre-overlay press must not poison the pairing (as in
                // `note_consumed_press`).
                if let InputEvent::Key {
                    key, event_type, ..
                } = &ev
                    && matches!(event_type, aterm_types::keyboard::KeyEventType::Press)
                    && let Some(ws) = self.windows.get_mut(&wid)
                {
                    ws.overlay_consumed_keys.insert(key.clone());
                }
                use crate::overlay::OverlayKind;
                let controller = matches!(src, Source::Controller);
                self.with_gesture_source(controller, |app| {
                    match app
                        .windows
                        .get(&wid)
                        .and_then(|ws| ws.overlay.as_ref())
                        .map(|o| o.kind())
                    {
                        Some(OverlayKind::Palette) => app.palette_input_event(wid, &ev),
                        Some(OverlayKind::Overview) => {
                            app.overview_input_event(wid, &ev);
                        }
                        None => {}
                    }
                });
                return InputOutcome::Ok;
            }
        }
        if target_session.is_none()
            && self.with_gesture_source(matches!(src, Source::Controller), |app| {
                app.tab_menu_input_event(wid, &ev)
            })
        {
            if let InputEvent::Key {
                key, event_type, ..
            } = &ev
                && matches!(event_type, aterm_types::keyboard::KeyEventType::Press)
                && let Some(ws) = self.windows.get_mut(&wid)
            {
                ws.overlay_consumed_keys.insert(key.clone());
            }
            return InputOutcome::Ok;
        }
        if target_session.is_none() && self.autopilot_menu_input_event(wid, &ev) {
            if let InputEvent::Key {
                key, event_type, ..
            } = &ev
                && matches!(event_type, aterm_types::keyboard::KeyEventType::Press)
                && let Some(ws) = self.windows.get_mut(&wid)
            {
                ws.overlay_consumed_keys.insert(key.clone());
            }
            return InputOutcome::Ok;
        }
        if target_session.is_none() && self.rename_input_event(wid, &ev) {
            if let InputEvent::Key {
                key, event_type, ..
            } = &ev
                && matches!(event_type, aterm_types::keyboard::KeyEventType::Press)
                && let Some(ws) = self.windows.get_mut(&wid)
            {
                ws.overlay_consumed_keys.insert(key.clone());
            }
            return InputOutcome::Ok;
        }
        let typed = match &ev {
            InputEvent::Key {
                key, event_type, ..
            } => {
                !matches!(event_type, aterm_types::keyboard::KeyEventType::Release)
                    && !aterm_types::keyboard::is_modifier_or_lock_key(key)
            }
            InputEvent::Text(_) | InputEvent::Paste(..) => true,
            _ => false,
        };
        let arrival =
            (target_session.is_none() && typed).then(crate::metrics::native_input_arrival);
        if target_session.is_none()
            && self.with_gesture_source(matches!(src, Source::Controller), |app| {
                app.timed_native_input_event(wid, &ev, arrival)
            })
        {
            return InputOutcome::Ok;
        }
        let pointer = matches!(
            ev,
            InputEvent::MouseButton { .. }
                | InputEvent::MouseMove { .. }
                | InputEvent::Wheel { .. }
        );
        let wid = if let Some(target) = target_session {
            let presented = self
                .front_terminal(wid)
                .filter(|terminal| terminal.session == target)
                .map(|_| wid)
                .or_else(|| {
                    (pointer && self.visible_session_rect(wid, target).is_some()).then_some(wid)
                })
                .or_else(|| {
                    self.windows.keys().copied().find(|candidate| {
                        self.front_terminal(*candidate)
                            .is_some_and(|terminal| terminal.session == target)
                    })
                });
            let Some(presented) = presented else {
                // The hidden seam carries the keyboard and paste payloads it
                // was built for. A pinned control request whose session left
                // the front between its check and now (`control::InputRoute`)
                // can also carry a view-bound gesture: focus, pointer, viewport
                // scroll or geometry. With no view of the session there is
                // nothing for it to act on, so it is dropped rather than
                // retargeted to whatever the window shows now, and rather
                // than being classed a typing press that snaps and deselects
                // the hidden session.
                if !matches!(
                    ev,
                    InputEvent::Key { .. }
                        | InputEvent::Text(_)
                        | InputEvent::KeySequence(_)
                        | InputEvent::Paste(..)
                ) {
                    return InputOutcome::Ok;
                }
                self.note_person_input(target, &ev, src);
                return self.input_to_hidden_session(target, ev, phase, completion);
            };
            presented
        } else {
            wid
        };
        self.reconcile_licence(wid);
        let terminal = match target_session {
            Some(session) => self.pool.get(session).map(|owner| {
                (
                    owner.term.clone(),
                    owner.ctx.sink.clone(),
                    owner.ctx.output_echo.clone(),
                    owner.ctx.modes.clone(),
                    owner.ctx.ui_waiting.clone(),
                    session,
                )
            }),
            None => self.front_terminal(wid).and_then(|terminal| {
                self.pool.get(terminal.session).map(|owner| {
                    (
                        terminal.term.clone(),
                        terminal.sink.clone(),
                        owner.ctx.output_echo.clone(),
                        owner.ctx.modes.clone(),
                        owner.ctx.ui_waiting.clone(),
                        terminal.session,
                    )
                })
            }),
        };
        let Some((term, sink, output_echo, modes, ui_waiting, session)) = terminal else {
            return match ev {
                InputEvent::Resize {
                    rows,
                    cols,
                    echo_to_window,
                } => self.input_resize(wid, rows, cols, echo_to_window),
                InputEvent::ResizeWindowPx { width, height } => {
                    self.input_resize_window_px(wid, width, height)
                }
                InputEvent::Focus(_) => InputOutcome::Ok,
                _ => InputOutcome::Ok,
            };
        };
        let completion = completion.filter(|c| {
            self.pool
                .get(session)
                .is_some_and(|s| c.matches(session, &s.ctx))
        });
        self.note_person_input(session, &ev, src);
        let input_now = match &ev {
            InputEvent::Key {
                event_type: aterm_types::keyboard::KeyEventType::Release,
                ..
            } => None,
            InputEvent::Key { .. }
            | InputEvent::Text(_)
            | InputEvent::KeySequence(_)
            | InputEvent::Paste(..) => Some(input_clock()),
            _ => None,
        };
        match ev {
            ev @ (InputEvent::Key { .. } | InputEvent::Text(_) | InputEvent::KeySequence(_)) => {
                let is_release = matches!(
                    &ev,
                    InputEvent::Key { event_type, .. }
                        if matches!(event_type, aterm_types::keyboard::KeyEventType::Release)
                );
                debug_assert_eq!(input_now.is_some(), !is_release);
                let folded_gate = ev.keypad_folded();
                let press_class = classify_press(&ev);
                let line_edit_key = press_class.edits_the_line();
                let PressClass {
                    console_input,
                    predict_candidate,
                    typed_forward,
                    navigation_key,
                    kill_key,
                    kill_moves,
                    kill_word,
                    line_abort,
                    job_chord,
                    redraw_chord,
                    enter_like,
                    keyed_enter,
                    line_terminator,
                    tab_key,
                    insert_tab,
                    paste_chord_key,
                    typed,
                    ime,
                    backspace,
                    brk,
                    inert_modifier,
                    spacebar,
                    shift_key,
                    glyph_shifted,
                } = press_class;
                let mut pending_key_sound = None;
                let is_repeat = phase == PressPhase::Repeat
                    || matches!(
                        &ev,
                        InputEvent::Key { event_type, .. }
                            if matches!(event_type, aterm_types::keyboard::KeyEventType::Repeat)
                    );
                let press_kind = PressKind::of(is_release, is_repeat, inert_modifier);
                let tty_swallows = input_now.is_some()
                    && (typed_forward == Some(true) || insert_tab || paste_chord_key)
                    && sink.tty_swallows_input();
                let shift_lift_withheld = input_now.is_some()
                    && shift_key
                    && press_kind == PressKind::Inert
                    && sink.tty_swallows_input();
                let mut swallowed_press = false;
                let mut composer_newline = false;
                if let Some(input_now) = input_now {
                    // A bare modifier is inert and leaves the blink phase alone. Gated on
                    // `inert_modifier` only, not on disturbing: an auto-repeat tick is a
                    // key still held, and the cursor stays solid for the whole hold.
                    if !inert_modifier {
                        self.reset_blink(wid);
                    }
                    let pmode = self.predict_mode();
                    let act: Option<PredictKey> = if pmode == crate::predict::PredictMode::Off {
                        None
                    } else {
                        predict_candidate
                    };
                    let pediting = self.config.predictive_echo_editing_or_default();
                    let typed_enter = is_plain_enter(&ev);
                    let mut key_row = match act {
                        Some(PredictKey::Glyph(_)) => self
                            .windows
                            .get_mut(&wid)
                            .filter(|ws| ws.predictor.wants_key_row())
                            .map(|ws| std::mem::take(&mut ws.predict_key_row)),
                        Some(PredictKey::Edit(_)) if pediting => self
                            .windows
                            .get_mut(&wid)
                            .map(|ws| std::mem::take(&mut ws.predict_key_row)),
                        Some(PredictKey::Edit(_)) | None => None,
                    };
                    let kitty_keyed_arrow = matches!(
                        act,
                        Some(PredictKey::Edit(
                            crate::predict::EditKey::Left
                                | crate::predict::EditKey::Right
                                | crate::predict::EditKey::Home
                                | crate::predict::EditKey::End
                                | crate::predict::EditKey::Delete
                        ))
                    ) && matches!(
                        &ev,
                        InputEvent::Key { key, mods, .. } if mods.intersects(
                            aterm_types::keyboard::Modifiers::CAPS_LOCK
                                | aterm_types::keyboard::Modifiers::NUM_LOCK
                        ) || matches!(
                            key,
                            aterm_types::keyboard::Key::Named(
                                aterm_types::keyboard::NamedKey::NumpadArrowLeft
                                    | aterm_types::keyboard::NamedKey::NumpadArrowRight
                            )
                        )
                    );
                    let edit_swallowed =
                        matches!(act, Some(PredictKey::Edit(_))) && sink.tty_swallows_input();
                    let kitty_cursor_enabled = match self.kitty_cursor_enabled_cache {
                        Some(n) => n,
                        None => {
                            let n = crate::app_render::ordinary_kitty_cursor_enabled(
                                self.config.cursor_trail_drawn(),
                                self.glow_style(),
                            );
                            self.kitty_cursor_enabled_cache = Some(n);
                            n
                        }
                    };
                    let click_audible = keystroke_click_audible(
                        if self.trail_audio.is_live() {
                            crate::trail_audio::HostState::Running
                        } else {
                            crate::trail_audio::HostState::Inert
                        },
                        self.config.trail_sounds_or_default(),
                        self.config.trail_sound_volume(),
                        self.serious_mode_policy()
                            .allows(crate::motion::SeriousEffect::TerminalSound),
                        self.windows
                            .get(&wid)
                            .is_some_and(|ws| ws.resize_sound_quiet(input_now)),
                    );
                    let click_synth = click_audible.then(|| KeyClickSynth {
                        style: self.glow_style(),
                        voice: self.trail_gesture_voice(),
                        gain: self.config.trail_sound_volume(),
                        bed: self.config.trail_sound_bed_or_default(),
                        tone_melody: self.config.tone_melody_or_default(),
                    });
                    let insert_gesture_armed = (insert_tab || paste_chord_key)
                        && !tty_swallows
                        && matches!(
                            self.glow_style(),
                            crate::cursor_glow::GlowStyle::RainbowKitty
                        );
                    if !inert_modifier && let Some(ws) = self.windows.get_mut(&wid) {
                        ws.input_hot = true;
                        ws.input_hot_until = Some(input_now + INPUT_HOT_WINDOW);
                        ws.last_key_at = Some(input_now);
                        crate::metrics::note_window_input(&mut ws.present_metrics.pending_input);
                        crate::input_hint::arm();
                    }
                    let (
                        scrolled,
                        cleared,
                        sample,
                        is_alt,
                        kitty_owns_keyboard,
                        term_cols,
                        ambiguous_width_double,
                        shell_executing,
                        submit_prompt,
                    ) = {
                        let site = crate::metrics::TermWaitSite::Press;
                        let mut t = crate::term_lock_ui(&term, &ui_waiting, site);
                        let (scrolled, cleared) = apply_press_custody(&mut t, press_kind);
                        let kitty_owns_keyboard = t.kitty_suppresses_predictive_echo();
                        let sample = act.is_some().then(|| {
                            let kitty_fn = t.kitty_reports_functional_keys();
                            let escaped_chords = t.control_chords_escaped();
                            (
                                t.cursor(),
                                t.content_scroll_state().coordinate_generation(),
                                (t.cols(), t.rows()),
                                t.is_alternate_screen() || kitty_owns_keyboard,
                                kitty_keyed_arrow && kitty_fn,
                                (
                                    t.input_region_start().map(|s| crate::predict::InputStart {
                                        cell: (s.row, s.col),
                                        prompt: s.prompt,
                                    }),
                                    crate::predict::EditorContext::new(
                                        session,
                                        t.shell_integration_nonce_in_use().as_ref(),
                                        t.modes().application_cursor_keys,
                                        escaped_chords,
                                    ),
                                    escaped_chords,
                                    t.grid().pending_wrap(),
                                ),
                            )
                        });
                        if let (Some(row), Some((cursor, ..))) = (key_row.as_mut(), sample.as_ref())
                        {
                            crate::predict_seen::seen_row(&t, cursor.row, row);
                        }
                        let is_alt = t.is_alternate_screen();
                        let term_cols = t.cols();
                        let ambiguous_width_double = t.modes().ambiguous_width_double;
                        let shell_executing =
                            typed_enter && crate::quit_safety::shell_executing(&t);
                        let submit_prompt = line_terminator.then(|| t.prompt_in_front()).flatten();
                        if crate::app_render::trace_spawn_enabled() && typed_forward.is_some() {
                            crate::logging::stderr_line!(
                                "PRESS fwd={typed_forward:?} typed={typed:?} gen={}",
                                t.pipeline_timestamps().process_sequence,
                            );
                        }
                        (
                            scrolled,
                            cleared,
                            sample,
                            is_alt,
                            kitty_owns_keyboard,
                            term_cols,
                            ambiguous_width_double,
                            shell_executing,
                            submit_prompt,
                        )
                    };
                    publish_release_relevance(session, kitty_owns_keyboard);
                    if (scrolled || cleared)
                        && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref())
                    {
                        w.request_redraw();
                    }
                    if let Some(ws) = self.windows.get_mut(&wid) {
                        let typed_press_supersede = typed_forward == Some(true)
                            && (!enter_like || (enter_like && !typed_enter && is_alt));
                        if inert_modifier {
                        } else if typed_press_supersede || typed_enter || tab_key || paste_chord_key
                        {
                            ws.fx.cursor_mut().glow.supersede_typed_press();
                        } else if line_edit_key {
                            ws.fx.cursor_mut().glow.clear_typed_keeping_hand(input_now);
                            ws.fx.cursor_mut().trail.clear_typed();
                        } else {
                            ws.fx.cursor_mut().glow.clear_typed(input_now);
                            ws.fx.cursor_mut().trail.clear_typed();
                        }
                        if !navigation_key && !kill_key && !inert_modifier {
                            ws.fx.cursor_mut().cadence.on_keystroke(input_now);
                        }
                        if typed_forward == Some(false) {
                            let erased = ws.erase_prices.pop();
                            ws.fx
                                .cursor_mut()
                                .glow
                                .note_backspace_erasing(input_now, erased);
                        }
                        let shift_enter_insert = enter_like && !typed_enter && is_alt;
                        let prices_typed_glyphs = typed_forward == Some(true)
                            && (!enter_like || shift_enter_insert)
                            && !tty_swallows;
                        if typed_forward != Some(false) && !inert_modifier && !prices_typed_glyphs {
                            ws.erase_prices.clear();
                        }
                        if shift_enter_insert {
                            ws.fx.cursor_mut().glow.note_newline_break(input_now);
                            composer_newline = true;
                        }
                        if typed_forward == Some(true) && (!enter_like || shift_enter_insert) {
                            let typed_cells: u16 = ime
                                .map(|text| committed_text_cells(text, ambiguous_width_double))
                                .or_else(|| {
                                    typed.map(|ch| committed_char_cells(ch, ambiguous_width_double))
                                })
                                .unwrap_or(1)
                                .max(1);
                            ws.fx.cursor_mut().momentum.on_key(
                                input_now,
                                aterm_effects::cursor_momentum::MOMENTUM_GLOW_TAU_S,
                            );
                            if tty_swallows {
                                swallowed_press = true;
                            } else {
                                if let Some(ch) =
                                    typed.filter(|_| ime.is_none() && typed_cells == 1)
                                {
                                    ws.fx.cursor_mut().glow.note_typed_expected(
                                        input_now,
                                        typed_cells,
                                        glyph_shifted && !spacebar,
                                        typed_class_for(typed),
                                        ch,
                                    );
                                } else {
                                    ws.fx.cursor_mut().glow.note_typed_glyph(
                                        input_now,
                                        typed_cells,
                                        glyph_shifted && !spacebar,
                                        typed_class_for(typed),
                                    );
                                }
                                match ime {
                                    Some(text) => ws
                                        .erase_prices
                                        .price_graphemes(text, ambiguous_width_double),
                                    None => ws.erase_prices.push(typed_cells),
                                }
                                ws.fx.cursor_mut().trail.note_typed(input_now);
                            }
                            let committed_capital = ime.is_some_and(|text| {
                                let mut glyphs = text.chars();
                                matches!(
                                    (glyphs.next(), glyphs.next()),
                                    (Some(ch), None) if ch.is_uppercase()
                                )
                            });
                            let stamp = key_click_stamp(
                                tty_swallows,
                                spacebar,
                                glyph_shifted
                                    || typed.is_some_and(char::is_uppercase)
                                    || committed_capital,
                                typed,
                            );
                            if click_audible
                                && ws.fx.cursor_mut().glow.cue_keystroke_shifted(
                                    input_now,
                                    stamp.kind,
                                    stamp.shifted,
                                )
                            {
                                let delivered = match (
                                    click_synth.as_ref(),
                                    ws.fx.cursor_mut().glow.take_key_cue(),
                                ) {
                                    (Some(synth), Some(cue)) => {
                                        pending_key_sound = Some((
                                            cue,
                                            *synth,
                                            term_cols,
                                            stamp.glyph_class,
                                            stamp.rank,
                                            key_sound_at_ms(),
                                        ));
                                        true
                                    }
                                    _ => false,
                                };
                                if !delivered && let Some(w) = ws.os_window.as_ref() {
                                    w.request_redraw();
                                }
                            }
                        }
                        if shift_key
                            && press_kind == PressKind::Inert
                            && !shift_lift_withheld
                            && click_audible
                            && ws.fx.cursor_mut().glow.cue_modifier(input_now)
                        {
                            // `take_key_cue` pops the lift whether or not it can be
                            // delivered, which is the drop policy: an undeliverable
                            // lift must not be replayed stale by the next drain.
                            if let (Some(synth), Some(cue)) =
                                (click_synth.as_ref(), ws.fx.cursor_mut().glow.take_key_cue())
                            {
                                pending_key_sound =
                                    Some((cue, *synth, term_cols, 0, 0, key_sound_at_ms()));
                            }
                        }
                        if kill_key {
                            // The companion's delete "oops" fires for span kills too
                            // (Ctrl-K/U/W, Alt-D, forward Delete, word-backspace): they
                            // erase text as surely as Backspace. Paid only while the
                            // kitty cursor is enabled or a companion is live; `on_kill`
                            // never summons.
                            if kitty_cursor_enabled || ws.fx.companions_mut().cursor_cat.is_active()
                            {
                                ws.fx.companions_mut().cursor_cat.on_kill(input_now);
                            }
                        }
                        if typed_enter && !shift_enter_insert {
                            if shell_executing || is_alt {
                                ws.fx.cursor_mut().glow.note_return_keeping_hand(input_now);
                            } else {
                                ws.fx.cursor_mut().glow.note_return(input_now);
                            }
                            ws.fx.cursor_mut().trail.note_return(input_now);
                        }
                        if (tab_key || paste_chord_key) && !tty_swallows {
                            ws.fx.cursor_mut().glow.note_user_gesture(input_now);
                            ws.fx.cursor_mut().trail.note_user_gesture(input_now);
                        }
                        if insert_gesture_armed {
                            ws.fx
                                .cursor_mut()
                                .glow
                                .note_insert_armed(input_now, InsertWidth::Unknown);
                        }
                        if navigation_key {
                            ws.fx.cursor_mut().glow.note_motion(input_now);
                            ws.fx.cursor_mut().trail.note_navigation(input_now);
                        }
                        if kill_key && kill_word {
                            ws.fx
                                .cursor_mut()
                                .glow
                                .note_word_kill(input_now, kill_moves);
                        } else if kill_key {
                            ws.fx.cursor_mut().glow.note_kill(input_now, kill_moves);
                        }
                        if typed_forward == Some(false)
                            && (kitty_cursor_enabled
                                || ws.fx.companions_mut().cursor_cat.is_active())
                        {
                            ws.fx.companions_mut().cursor_cat.on_key(input_now, false);
                        }
                        if typed_enter {
                            ws.fx.companions_mut().cursor_cat.on_enter(input_now);
                        }
                        if predict_candidate.is_none() && !inert_modifier {
                            let was_displaying = ws.predictor.is_displaying(input_now);
                            if line_terminator {
                                ws.predictor.note_line_submit_at(submit_prompt, input_now);
                            } else {
                                ws.predictor.note_unmodelled_input(input_now);
                            }
                            ws.present_metrics.pending_input.tag_newest(None);
                            #[cfg(any(test, debug_assertions))]
                            crate::predict_trace::press(
                                &ws.predictor,
                                if line_terminator {
                                    "submit"
                                } else {
                                    "unmodelled"
                                },
                                None,
                                input_now,
                            );
                            if prediction_visibility_requires_redraw(
                                was_displaying,
                                ws.predictor.is_displaying(input_now),
                            ) && let Some(w) = ws.os_window.as_ref()
                            {
                                w.request_redraw();
                            }
                        }
                        if let Some(rain) = ws.fx.rain_mut() {
                            rain.note_keystroke();
                            if typed_enter {
                                rain.note_signal(
                                    crate::matrix_rain::RainSignal::TurnStart as u32,
                                    4,
                                );
                            }
                            if is_alt
                                && matches!(
                                    folded_gate.as_ref().unwrap_or(&ev),
                                    InputEvent::Key {
                                        key: aterm_types::keyboard::Key::Named(
                                            aterm_types::keyboard::NamedKey::PageUp
                                                | aterm_types::keyboard::NamedKey::PageDown
                                        ),
                                        ..
                                    }
                                )
                            {
                                rain.note_alt_scroll();
                            }
                            if !rain.is_active()
                                && rain.notes_can_wake()
                                && let Some(w) = ws.os_window.as_ref()
                            {
                                w.request_redraw();
                            }
                        }
                    }
                    if let Some((
                        key,
                        (
                            cur,
                            coords,
                            (cols, rows),
                            no_echo,
                            chord_arrow,
                            (input_start, context, escaped_chords, wrap_pending),
                        ),
                    )) = act.zip(sample)
                    {
                        // Split panes predict too: `sync_window` resets the predictor
                        // when the focused pane or tab changes.
                        if let Some(ws) = self.windows.get_mut(&wid) {
                            let now = input_now;
                            let was_displaying = ws.predictor.is_displaying(now);
                            let op_before = ws.predictor.newest_op();
                            #[cfg(any(test, debug_assertions))]
                            let traced_key = if no_echo || tty_swallows || edit_swallowed {
                                "unmodelled no-echo".to_string()
                            } else if chord_arrow {
                                "unmodelled chord-arrow".to_string()
                            } else {
                                match key {
                                    PredictKey::Glyph(c) => format!("glyph {c}"),
                                    PredictKey::Edit(k) => format!("edit {k:?}"),
                                }
                            };
                            if no_echo || tty_swallows || edit_swallowed {
                                // A no-echo press arms nothing, but it is still sent and the
                                // program may echo it after all.
                                ws.predictor.note_unmodelled_input(now);
                            } else if chord_arrow {
                                // A modified arrow (a lock in Kitty's modifier byte) is
                                // unmodelled input.
                                ws.predictor.note_unmodelled_input(now);
                            } else {
                                ws.predictor.set_mode(pmode);
                                ws.predictor.set_editing(pediting);
                                ws.predictor
                                    .set_ambiguous_width_double(ambiguous_width_double);
                                let sample = crate::predict::KeySample {
                                    cursor: (cur.row, cur.col),
                                    coords,
                                    grid: (cols, rows),
                                    row: key_row.as_ref().map(|row| row.cells()),
                                    below: key_row.as_ref().and_then(|row| row.below()),
                                    input_start,
                                    context,
                                    escaped_chords,
                                    wrap_pending: Some(wrap_pending),
                                };
                                match key {
                                    PredictKey::Glyph(c) => {
                                        let _armed = ws.predictor.predict_glyph(c, &sample, now);
                                    }
                                    PredictKey::Edit(k) => {
                                        let _outcome = ws.predictor.predict_edit(k, &sample, now);
                                    }
                                }
                            }
                            let armed = ws.predictor.armed_since(op_before);
                            ws.present_metrics.pending_input.tag_newest(armed);
                            #[cfg(any(test, debug_assertions))]
                            crate::predict_trace::press(&ws.predictor, &traced_key, armed, now);
                            if let Some(row) = key_row.take() {
                                ws.predict_key_row = row;
                            }
                            if prediction_visibility_requires_redraw(
                                was_displaying,
                                ws.predictor.is_displaying(now),
                            ) && let Some(w) = ws.os_window.as_ref()
                            {
                                w.request_redraw();
                            }
                        }
                    }
                } else {
                    {
                        let site = crate::metrics::TermWaitSite::Press;
                        let mut t = crate::term_lock_ui(&term, &ui_waiting, site);
                        let _ = apply_press_custody(&mut t, press_kind);
                    }
                };
                let delivery_ticket = input_now.map(|dispatched| DeliveryTicket {
                    insert: (insert_tab || paste_chord_key)
                        .then_some(InsertWidth::Unknown)
                        .filter(|_| !tty_swallows),
                    window: Some(wid),
                    key: if typed_forward == Some(true) && !enter_like && !tty_swallows {
                        Some(DeliveredClass::Typed)
                    } else if is_plain_enter(&ev) {
                        Some(DeliveredClass::Return)
                    } else if composer_newline {
                        Some(DeliveredClass::Newline)
                    } else if navigation_key {
                        Some(DeliveredClass::Nav)
                    } else if typed_forward == Some(false) {
                        Some(DeliveredClass::Erase)
                    } else if kill_key {
                        Some(DeliveredClass::Kill {
                            moves_cursor: kill_moves,
                            word: kill_word,
                        })
                    } else if (tab_key || paste_chord_key) && !tty_swallows {
                        Some(DeliveredClass::Gesture)
                    } else {
                        None
                    },
                    dispatched_at: dispatched,
                });
                let (receipt, wrote_inline) = paste_order::ordered_or_inline_observed(
                    &term,
                    &modes,
                    &sink,
                    &output_echo,
                    &ev,
                    delivery_ticket,
                    (!inert_modifier && !is_release && !tty_swallows)
                        .then_some((session, self.proxy.as_ref())),
                    completion,
                );
                let outcome = receipt_outcome(receipt);
                if receipt.input_queue_full() {
                    self.refuse_input(session, InputRefusal::Busy);
                }
                if swallowed_press
                    && !outcome.failed()
                    && let Some(ws) = self.windows.get_mut(&wid)
                {
                    ws.fx.cursor_mut().glow.note_typed_swallowed_no_echo();
                }
                if outcome == InputOutcome::Ok
                    && (!wrote_inline || receipt.accepted_order().is_some())
                    && let (Some(now), Some(kind)) = (input_now, console_input)
                    && let Some(ws) = self.windows.get_mut(&wid)
                {
                    ws.fx.companion_mut().note_console_input(now, kind);
                }
                if (!wrote_inline || outcome.failed())
                    && let Some(armed_at) = input_now
                    && let Some(ws) = self.windows.get_mut(&wid)
                {
                    revoke_dispatch(ws, armed_at, outcome.failed());
                }
                if !is_release {
                    if wrote_inline && receipt.accepted_order().is_some() {
                        crate::metrics::note_pty_write();
                    } else {
                        crate::metrics::clear_key_arrival();
                    }
                }
                if let Some((cue, synth, cols, glyph_class, rank, at_ms)) = pending_key_sound
                    && let Some(ws) = self.windows.get(&wid)
                {
                    let policy = crate::app_render::TrailSoundPolicy {
                        voice: synth.voice,
                        gain: Some(synth.gain),
                        tone: ws.fx.companions().tone_tracker.effective(synth.tone_melody),
                        bed: synth.bed,
                    };
                    self.trail_audio.push_meta(
                        crate::app_render::trail_sound_event(
                            &cue,
                            synth.style,
                            cols,
                            &policy,
                            synth.gain,
                        ),
                        aterm_effects::trail_sound::EventMeta {
                            at_ms,
                            glyph_class,
                            rank,
                            pan_from: 0.0,
                            block_lead_s: 0.0,
                            flow: ws.fx.cursor().glow.flow_status().heat,
                        },
                    );
                }
                if let Some(input_now) = input_now {
                    let wire_at = if wrote_inline {
                        input_now
                    } else {
                        input_now + ORDERED_EGRESS_WITNESS_GRACE
                    };
                    let cosmetic_press = aterm_effects::companions::TypedPress {
                        brk,
                        backspace,
                        typed,
                        ime,
                    };
                    let tone_active = self.tone_infer_active();
                    let lexicon = self.sparkle.as_ref().map(|rs| &rs.lexicon);
                    let opts = self
                        .sparkle
                        .as_ref()
                        .map(|rs| rs.cfg.scan_opts())
                        .unwrap_or_default();
                    let summoned = match self.windows.get_mut(&wid) {
                        Some(ws) => {
                            // The typed-edit witness (above), armed first: edits only
                            // (walking the caret over a word is not retyping it), scoped to
                            // this pane's session so it never re-arms another pane.
                            if typed_forward.is_some() || kill_key {
                                ws.fx.decos_mut().note_typed_edit(wire_at, Some(session));
                            }
                            ws.fx.companions_mut().note_typed_press(
                                cosmetic_press,
                                aterm_effects::companions::PressFeed {
                                    session,
                                    now: input_now,
                                    words: lexicon.map(|lx| (&**lx, &opts)),
                                    tone_active,
                                    keyed_enter,
                                },
                            )
                        }
                        None => aterm_effects::kitty_summon::TypedHit::default(),
                    };
                    if summoned.records {
                        self.record_typed_kitty(wid, session, input_now);
                    }
                    if summoned.feline || summoned.profanity {
                        self.react_typed_word(wid, input_now, summoned);
                    }
                    if summoned.canine {
                        self.summon_typed_dog(wid, session, input_now);
                    }
                    let pet_note = {
                        let tricks = self.prepared_sparkle.tricks();
                        let press = TrickPress {
                            tty_swallows,
                            enter_like,
                            keyed_enter,
                            line_abort,
                            job_chord,
                            redraw_chord,
                            kill_key,
                            kill_moves,
                            kill_word,
                            brk,
                            backspace,
                            typed,
                            ime,
                        };
                        let live = self.config.kitty_tricks_enabled()
                            && self
                                .serious_mode_policy()
                                .allows(crate::motion::SeriousEffect::CursorCat);
                        let flash_on = self.sparkle.is_some();
                        match self.windows.get_mut(&wid) {
                            Some(ws) => {
                                ws.trick_fire = ws.trick_fire.for_session(session);
                                let TrickFeed {
                                    event,
                                    submit_pet_only,
                                } = feed_trick_listener(
                                    &mut ws.trick_listener,
                                    tricks,
                                    session,
                                    input_now,
                                    press,
                                );
                                let mut pet_note = None;
                                match event {
                                    Some(TrickEvent::Fire {
                                        trick,
                                        word,
                                        commands,
                                        back_chars,
                                        addressed,
                                    }) => {
                                        let admitted = live
                                            && !self
                                                .config
                                                .kitty_trick_ignored(word, commands.iter());
                                        ws.trick_fire = if admitted {
                                            TrickLineFire::Admitted { session, addressed }
                                        } else {
                                            TrickLineFire::Withheld { session }
                                        };
                                        if admitted {
                                            // The flash, best-effort: it needs a running
                                            // word engine to find the word. Noted now,
                                            // before the boundary key's echo is scanned,
                                            // in the key's pane, stamped against the wire.
                                            if flash_on {
                                                ws.fx.decos_mut().note_trick_typed(
                                                    wire_at,
                                                    Some(session),
                                                    word,
                                                    back_chars,
                                                );
                                            }
                                            pet_note = Some((trick, addressed));
                                        }
                                    }
                                    Some(TrickEvent::Confirm { trick }) => {
                                        // Only an admitted fire can be confirmed into the
                                        // pet: a withheld one (ignored word, feature off)
                                        // must not latch by the back door, nor an admitted
                                        // one whose gate closed since (a config reload
                                        // mid-line). The gate is read at every event.
                                        if live
                                            && let TrickLineFire::Admitted { .. } = ws.trick_fire
                                        {
                                            ws.trick_fire = TrickLineFire::Admitted {
                                                session,
                                                addressed: true,
                                            };
                                            pet_note = Some((trick, true));
                                        }
                                    }
                                    Some(TrickEvent::Revoke) => revoke_trick_line(ws, input_now),
                                    None => {}
                                }
                                if submit_pet_only
                                    && live
                                    && !matches!(ws.trick_fire, TrickLineFire::Withheld { .. })
                                {
                                    ws.fx.companion_mut().note_trick_submit(input_now);
                                }
                                if press.line_ended() || ws.trick_listener.line_chars() == 0 {
                                    ws.trick_fire = TrickLineFire::None;
                                }
                                pet_note
                            }
                            None => None,
                        }
                    };
                    if let Some((trick, confirmed)) = pet_note
                        && self.trail_is_kitty_pet()
                        && let Some(ws) = self.windows.get_mut(&wid)
                    {
                        ws.fx
                            .companion_mut()
                            .note_trick(input_now, trick, confirmed);
                        if let Some(w) = ws.os_window.as_ref() {
                            w.request_redraw();
                        }
                    }
                }
                outcome
            }
            ev @ InputEvent::MouseButton { .. } => {
                let fronted = self.session_is_front(wid, session);
                self.input_mouse_button(wid, &ev, &term, &modes, &sink, fronted)
            }
            ev @ InputEvent::MouseMove { .. } => {
                let (row, col, side) = if let InputEvent::MouseMove { row, col, side, .. } = ev {
                    (row, col, side)
                } else {
                    unreachable!()
                };
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.last_mouse_side = side;
                }
                if self.windows.get(&wid).is_some_and(|ws| ws.selecting) {
                    self.clear_move_license(wid);
                    self.drag_selection(wid, row, col);
                    return InputOutcome::Ok;
                }
                let receipt = paste_order::report_ordered_or_inline(&term, &modes, &sink, &ev);
                note_refused_report(receipt);
                if self.session_is_front(wid, session)
                    && (receipt.accepted_order().is_some()
                        || receipt.is_queued()
                        || !matches!(
                            receipt.egress,
                            input::Egress::TrackingOff { .. }
                                | input::Egress::Reported(input::Delivery::Full)
                        ))
                {
                    self.clear_move_license(wid);
                }
                receipt_outcome(receipt)
            }
            ev @ InputEvent::Wheel { .. } => {
                self.input_wheel(wid, session, &ev, &term, &modes, &sink)
            }
            InputEvent::ScrollView(intent) => {
                // Toward history: a person reaching for scrollback that a rewrap
                // has detached is waiting on it.
                if matches!(
                    intent,
                    ScrollIntent::Up | ScrollIntent::Top | ScrollIntent::PrevPrompt
                ) || matches!(intent, ScrollIntent::By(n) if n > 0)
                {
                    self.note_history_wanted(session);
                }
                self.input_scroll_view(wid, intent, &term)
            }
            ev @ InputEvent::Paste(..) => {
                // A paste is typing for the cursor-effect wake (the text lands at
                // the caret and the cursor walks it), so it stamps `last_key_at` as
                // the key arms do, or `ctl paste` into an unfocused window would
                // paint nothing.
                if let Some(now) = input_now
                    && let Some(ws) = self.windows.get_mut(&wid)
                {
                    ws.last_key_at = Some(now);
                    ws.predictor.note_unmodelled_input(now);
                    ws.trick_listener.rekey(session);
                    ws.trick_fire = ws.trick_fire.for_session(session);
                    if ws.trick_listener.note_break().event == Some(TrickEvent::Revoke) {
                        revoke_trick_line(ws, now);
                    }
                }
                self.input_paste(
                    wid,
                    session,
                    ev,
                    &term,
                    &modes,
                    &sink,
                    &output_echo,
                    input_now.expect("paste dispatch owns an injected input timestamp"),
                    completion,
                )
            }
            InputEvent::Resize {
                rows,
                cols,
                echo_to_window,
            } => self.input_resize(wid, rows, cols, echo_to_window),
            InputEvent::ResizeWindowPx { width, height } => {
                self.input_resize_window_px(wid, width, height)
            }
            ev @ InputEvent::Focus(_) => {
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.fx
                        .cursor_mut()
                        .glow
                        .clear_typed(std::time::Instant::now());
                    ws.fx.cursor_mut().trail.clear_typed();
                }
                let receipt = paste_order::report_ordered_or_inline(&term, &modes, &sink, &ev);
                note_refused_report(receipt);
                InputOutcome::Ok
            }
        }
    }

    /// The `InputEvent::MouseButton` arm of [`App::input`].
    fn input_mouse_button(
        &mut self,
        wid: WindowId,
        ev: &InputEvent,
        term: &Arc<Mutex<Terminal>>,
        modes: &Arc<ModeMirror>,
        sink: &Arc<SinkWriter>,
        fronted: bool,
    ) -> InputOutcome {
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.fx
                .cursor_mut()
                .glow
                .clear_typed(std::time::Instant::now());
            ws.fx.cursor_mut().trail.clear_typed();
        }
        let (button, pressed, row, col, click_count, side, block, suppress_copy_on_select) =
            if let &InputEvent::MouseButton {
                button,
                pressed,
                row,
                col,
                click_count,
                side,
                block,
                suppress_copy_on_select,
                ..
            } = ev
            {
                (
                    button,
                    pressed,
                    row,
                    col,
                    click_count,
                    side,
                    block,
                    suppress_copy_on_select,
                )
            } else {
                unreachable!()
            };
        let receipt = paste_order::report_ordered_or_inline(term, modes, sink, ev);
        note_refused_report(receipt);
        let egress = receipt.egress;
        if let input::Egress::TrackingOff { .. } = egress
            && button == aterm_types::mouse::MouseButton::Left
            && fronted
        {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.last_mouse_cell = (row, col);
                ws.last_mouse_side = side;
            }
            if pressed {
                self.seam_left_press(wid, row, col, click_count, block);
            } else if self.windows.get(&wid).is_some_and(|ws| ws.selecting) {
                self.finish_selection(wid, suppress_copy_on_select);
            }
        }
        receipt_outcome(receipt)
    }

    /// The `InputEvent::Wheel` arm of [`App::input`].
    fn input_wheel(
        &mut self,
        wid: WindowId,
        session: u64,
        ev: &InputEvent,
        term: &Arc<Mutex<Terminal>>,
        modes: &Arc<ModeMirror>,
        sink: &Arc<SinkWriter>,
    ) -> InputOutcome {
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.fx
                .cursor_mut()
                .glow
                .clear_typed(std::time::Instant::now());
            ws.fx.cursor_mut().trail.clear_typed();
        }
        if let Some(rain) = self.windows.get_mut(&wid).and_then(|ws| ws.fx.rain_mut())
            && modes.alternate_screen()
        {
            rain.note_alt_scroll();
        }
        if let InputEvent::Wheel { mods, .. } = ev
            && let Some(dy_px) = self.windows.get(&wid).and_then(|ws| ws.wheel_precise_px)
            && !self.rewrap_in_flight(session)
            && self.precise_push_past_parked_edge(wid, term, dy_px, input_clock())
            && input::wheel_route_mirrored(modes, *mods) == input::WheelRoute::Viewport
        {
            self.scroll_wheel_animated_with(wid, term, 0, None, Some(dy_px));
            return InputOutcome::Ok;
        }
        let receipt = paste_order::report_ordered_or_inline(term, modes, sink, ev);
        note_refused_report(receipt);
        let egress = receipt.egress;
        if let input::Egress::TrackingOff {
            wheel_lines,
            wheel_up,
            viewport: Some(viewport),
        } = egress
        {
            let detent = matches!(ev, InputEvent::Wheel { detent: true, .. });
            let lines = wheel_viewport_lines(wheel_lines, viewport.rows, detent);
            if lines != 0 {
                // Toward history: a person reaching for scrollback a rewrap has
                // detached is waiting on it.
                if wheel_up {
                    self.note_history_wanted(session);
                }
                let delta = if wheel_up { lines } else { -lines };
                if self.rewrap_in_flight(session) {
                    let now = std::time::Instant::now();
                    self.settle_scroll_motion_at_target(wid, now);
                    term_lock(term).scroll_display(delta);
                    if let Some(ws) = self.windows.get_mut(&wid) {
                        ws.scroll_pill.touch(now);
                        if let Some(w) = ws.os_window.as_ref() {
                            w.request_redraw();
                        }
                    }
                } else {
                    let precise = self.windows.get(&wid).and_then(|ws| ws.wheel_precise_px);
                    self.scroll_wheel_animated_with(wid, term, delta, Some(viewport), precise);
                }
            }
        } else if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.wheel_precise_px.is_some())
        {
            self.release_scroll_track(wid);
        }
        receipt_outcome(receipt)
    }

    /// Whether a precise delta of `dy_px` (+ = into history) pushes a band parked on a history end
    /// further past it (down at the live bottom, up at the top).
    fn precise_push_past_parked_edge(
        &self,
        wid: WindowId,
        term: &Arc<Mutex<Terminal>>,
        dy_px: f64,
        now: std::time::Instant,
    ) -> bool {
        let cell_h = self.win_cell_size(wid).1.max(1) as i64;
        let Some(st) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.scroll_glide.as_ref())
            .filter(|st| Arc::ptr_eq(&st.term, term) && st.cell_h == cell_h)
        else {
            return false;
        };
        let (pos, _) = st.glide.sample(now);
        let past_live = dy_px < 0.0 && pos <= 0 && st.engine_row == 0;
        let past_top = dy_px > 0.0 && pos >= st.max_rows * cell_h && st.engine_row == st.max_rows;
        past_live || past_top
    }

    /// Settles every retained smooth-scroll artifact for one window at its intended whole-row
    /// target.
    pub(crate) fn settle_scroll_motion_at_target(
        &mut self,
        wid: WindowId,
        now: std::time::Instant,
    ) -> bool {
        let Some((glide, redraw, changed)) = self.windows.get_mut(&wid).map(|ws| {
            let glide = ws.scroll_glide.take();
            let overscroll = ws.overscroll.take();
            let changed = glide.is_some() || overscroll.is_some() || ws.scroll_frac_px != 0;
            if changed {
                ws.scroll_frac_px = 0;
                ws.scroll_pill.touch(now);
            }
            (glide, ws.os_window.clone(), changed)
        }) else {
            return false;
        };
        if !changed {
            return false;
        }

        if let Some(st) = glide {
            let (target_row, residual) =
                crate::scroll_motion::decompose(st.glide.target_px(), st.cell_h);
            debug_assert_eq!(
                residual, 0,
                "a wheel glide target must be an exact whole-row position"
            );
            let mut term = term_lock(&st.term);
            let max_row = i64::try_from(term.grid().scrollback_lines()).unwrap_or(i64::MAX);
            let machine =
                i64::try_from(term.grid().display_offset()).unwrap_or(i64::MAX) - st.engine_row;
            let target_row = if target_row == 0 {
                0
            } else {
                target_row.saturating_add(machine).clamp(0, max_row)
            };
            loop {
                let current = i64::try_from(term.grid().display_offset()).unwrap_or(i64::MAX);
                let delta = target_row - current;
                if delta == 0 {
                    break;
                }
                let step = delta.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
                term.scroll_display(step);
                let landed = i64::try_from(term.grid().display_offset()).unwrap_or(i64::MAX);
                if landed == current {
                    // The terminal's live clamp is authoritative.
                    break;
                }
            }
        }
        if let Some(window) = redraw {
            window.request_redraw();
        }
        true
    }

    /// Applies the settle only when this window's SmoothScroll policy is Reduced.
    pub(crate) fn settle_scroll_motion_if_reduced(
        &mut self,
        wid: WindowId,
        now: std::time::Instant,
    ) -> bool {
        if self.smooth_scroll_animates(wid) {
            return false;
        }
        self.settle_scroll_motion_at_target(wid, now)
    }

    /// Reconciles every retained window after a fact that can change the resolved SmoothScroll
    /// policy.
    pub(crate) fn settle_reduced_scroll_motion(&mut self, now: std::time::Instant) {
        let retained: Vec<WindowId> = self
            .windows
            .iter()
            .filter(|(_, ws)| {
                ws.scroll_glide.is_some() || ws.overscroll.is_some() || ws.scroll_frac_px != 0
            })
            .map(|(wid, _)| *wid)
            .collect();
        for wid in retained {
            self.settle_scroll_motion_if_reduced(wid, now);
        }
    }

    /// Install one live OS accessibility sample and reconcile every affected glide in the same
    /// event-loop turn.
    #[cfg(any(target_os = "macos", windows, test))]
    pub(crate) fn apply_system_reduce_motion(
        &mut self,
        reduced: bool,
        now: std::time::Instant,
    ) -> bool {
        if self.system_reduce_motion == reduced {
            return false;
        }
        self.system_reduce_motion = reduced;
        self.settle_reduced_scroll_motion(now);
        true
    }

    /// Test wrapper over [`Self::scroll_wheel_animated_at`] on the wall clock.
    #[cfg(test)]
    pub(crate) fn scroll_wheel_animated(
        &mut self,
        wid: WindowId,
        term: &Arc<Mutex<Terminal>>,
        delta_rows: i32,
    ) {
        let now = std::time::Instant::now();
        self.scroll_wheel_animated_at(wid, term, delta_rows, None, None, now);
    }

    /// Smooth scroll: moves the local viewport by `delta_rows`.
    pub(crate) fn scroll_wheel_animated_with(
        &mut self,
        wid: WindowId,
        term: &Arc<Mutex<Terminal>>,
        delta_rows: i32,
        viewport: Option<input::WheelViewport>,
        precise: Option<f64>,
    ) {
        let now = input_clock();
        self.scroll_wheel_animated_at(wid, term, delta_rows, viewport, precise, now);
    }

    /// [`Self::scroll_wheel_animated_with`] on an injected clock.
    pub(crate) fn scroll_wheel_animated_at(
        &mut self,
        wid: WindowId,
        term: &Arc<Mutex<Terminal>>,
        delta_rows: i32,
        viewport: Option<input::WheelViewport>,
        precise: Option<f64>,
        now: std::time::Instant,
    ) {
        let cell_h = self.win_cell_size(wid).1.max(1) as i64;
        let animate = self.smooth_scroll_animates(wid);
        if !animate {
            if !self.windows.contains_key(&wid) {
                return;
            }
            if delta_rows == 0 {
                return;
            }
            self.settle_scroll_motion_at_target(wid, now);
            term_lock(term).scroll_display(delta_rows);
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.scroll_pill.touch(now);
                if let Some(w) = ws.os_window.as_ref() {
                    w.request_redraw();
                }
            }
            return;
        }
        if self
            .windows
            .get(&wid)
            .and_then(|ws| ws.scroll_glide.as_ref())
            .is_some_and(|st| !Arc::ptr_eq(&st.term, term))
        {
            self.settle_scroll_motion_at_target(wid, now);
        }
        let app_interval = self.frame_interval;
        let chained = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.scroll_glide.as_ref())
            .filter(|st| Arc::ptr_eq(&st.term, term) && st.cell_h == cell_h)
            .map(|st| (st.engine_row, st.max_rows));
        let same = chained.is_some();
        let (cur_rows, max_rows) = match (viewport.as_ref(), chained, precise) {
            (Some(v), _, _) => (
                i64::try_from(v.display_offset).unwrap_or(i64::MAX),
                i64::try_from(v.scrollback_lines).unwrap_or(i64::MAX),
            ),
            (None, Some((engine_row, max_rows)), Some(_)) => (engine_row, max_rows),
            (None, _, _) => {
                let t = term_lock(term);
                (
                    i64::try_from(t.grid().display_offset()).unwrap_or(i64::MAX),
                    i64::try_from(t.grid().scrollback_lines()).unwrap_or(i64::MAX),
                )
            }
        };
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let tick_interval = ws.frame_interval.unwrap_or(app_interval);
        if let Some(dy_px) = precise {
            // Direct manipulation: the band goes where the finger is. A fresh gesture
            // parks a tracked glide at the engine's real row; a chained delta
            // continues whatever is armed from its current sample. The clamp applies
            // on every delta and its excess releases or stacks the bounce as a notch's
            // does. At an edge the tracked glide stays armed (start == target == edge
            // row) and the bounce owns `scroll_frac_px`, so a finger held past the top
            // re-probes the route once per settle, not per delta.
            if !same {
                ws.overscroll = None;
                ws.scroll_frac_px = 0;
                ws.scroll_glide = Some(crate::ScrollGlideState {
                    glide: crate::scroll_motion::Glide::tracked(cur_rows * cell_h, cell_h, now),
                    term: term.clone(),
                    cell_h,
                    engine_row: cur_rows,
                    max_rows,
                    next_tick: now,
                });
            }
            let (excess_px, edges) = ws.scroll_glide.as_mut().map_or((0, (false, false)), |st| {
                if viewport.is_some() {
                    st.max_rows = max_rows;
                }
                let max_px = st.max_rows * cell_h;
                let excess = st.glide.track(dy_px, cell_h, max_px, now);
                let (pos, _) = st.glide.sample(now);
                (excess, (pos >= max_px, pos <= 0))
            });
            let (at_top, at_live) = edges;
            self.apply_scroll_glide_sample(wid, now);
            let Some(ws) = self.windows.get_mut(&wid) else {
                return;
            };
            let stale = ws.overscroll.as_ref().is_some_and(|sp| {
                let d = sp.sample(now).0;
                (d <= 0.0 && !at_top) || (d >= 0.0 && !at_live)
            });
            if stale {
                ws.overscroll = None;
            }
            if excess_px != 0 {
                release_overscroll(ws, excess_px, cell_h, tick_interval, now);
            }
            if let Some(sp) = ws.overscroll.as_ref() {
                ws.scroll_frac_px = sp.sample(now).0.round() as i32;
            }
            ws.scroll_pill.touch(now);
            if let Some(w) = ws.os_window.as_ref() {
                w.request_redraw();
            }
            return;
        }
        let base_px = if same {
            ws.scroll_glide
                .as_ref()
                .map_or(0, |st| st.glide.target_px())
        } else {
            cur_rows * cell_h
        };
        let want_px = base_px + i64::from(delta_rows) * cell_h;
        let target_px = want_px.clamp(0, max_rows * cell_h);
        if same {
            ws.overscroll = None;
            if let Some(st) = ws.scroll_glide.as_mut() {
                st.glide.retarget(target_px, now);
                st.max_rows = max_rows;
                st.next_tick = st.next_tick.min(now + tick_interval);
            }
        } else if target_px == cur_rows * cell_h {
            ws.scroll_glide = None;
            release_overscroll(ws, want_px - target_px, cell_h, tick_interval, now);
        } else {
            ws.overscroll = None;
            ws.scroll_frac_px = 0;
            ws.scroll_glide = Some(crate::ScrollGlideState {
                glide: crate::scroll_motion::Glide::new(cur_rows * cell_h, target_px, now),
                term: term.clone(),
                cell_h,
                engine_row: cur_rows,
                max_rows,
                next_tick: now + tick_interval,
            });
        }
        ws.scroll_pill.touch(now);
        if let Some(w) = ws.os_window.as_ref() {
            w.request_redraw();
        }
    }

    /// One deadline wake of an in-flight glide ([`Self::service_due_scroll_motion`]).
    pub(crate) fn tick_scroll_glide(&mut self, wid: WindowId, now: std::time::Instant) {
        let animate = self.smooth_scroll_animates(wid);
        if !animate {
            self.settle_scroll_motion_at_target(wid, now);
            return;
        }
        self.apply_scroll_glide_sample(wid, now);
    }

    /// Presents the glide's sample at `now`.
    fn apply_scroll_glide_sample(&mut self, wid: WindowId, now: std::time::Instant) {
        let app_interval = self.frame_interval;
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let Some((term, cell_h, pos_px, mut done, engine_row)) =
            ws.scroll_glide.as_ref().map(|st| {
                let (pos, done) = st.glide.sample(now);
                (st.term.clone(), st.cell_h, pos, done, st.engine_row)
            })
        else {
            return;
        };
        let (row_floor, s) = crate::scroll_motion::decompose(pos_px, cell_h);
        let (offset, mut frac) = if s == 0 {
            (row_floor, 0)
        } else {
            (row_floor + 1, cell_h - s)
        };
        if offset != engine_row {
            let mut t = term_lock(&term);
            if t.grid().reflow_offload_in_flight() {
                drop(t);
                self.settle_scroll_motion_at_target(wid, now);
                return;
            }
            let cur = i64::try_from(t.grid().display_offset()).unwrap_or(i64::MAX);
            let machine = cur - engine_row;
            let live_bound = ws
                .scroll_glide
                .as_ref()
                .is_some_and(|st| st.glide.target_px() == 0);
            let offset = if machine < 0 || (machine > 0 && !live_bound) {
                if let Some(st) = ws.scroll_glide.as_mut() {
                    st.glide.shift(machine.saturating_mul(cell_h));
                    let shifted = pos_px.saturating_add(machine.saturating_mul(cell_h));
                    let target = st.glide.target_px();
                    if target < 0 || (target == 0 && shifted < 0) {
                        if shifted <= 0 {
                            done = true;
                            frac = 0;
                        } else {
                            st.glide.retarget(0, now);
                        }
                    }
                }
                offset + machine
            } else {
                offset
            };
            let delta = offset - cur;
            if delta != 0 {
                t.scroll_display(delta.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32);
            }
            let landed = i64::try_from(t.grid().display_offset()).unwrap_or(i64::MAX);
            if let Some(st) = ws.scroll_glide.as_mut() {
                st.engine_row = landed;
            }
        }
        ws.scroll_frac_px = i32::try_from(frac).unwrap_or(0);
        let tick_interval = ws.frame_interval.unwrap_or(app_interval);
        if let Some(st) = ws.scroll_glide.as_mut() {
            st.next_tick = if now < st.glide.settle_start() {
                st.glide.settle_start()
            } else {
                crate::app_render::phase_locked_effect_deadline(
                    now,
                    tick_interval,
                    Some(st.next_tick),
                    None,
                    true,
                )
            };
        }
        if done {
            ws.scroll_glide = None;
            ws.scroll_frac_px = 0;
        }
        ws.scroll_pill.touch(now);
        if let Some(w) = ws.os_window.as_ref() {
            w.request_redraw();
        }
    }

    /// Services every armed glide and bounce whose anchored tick is due at `now`.
    pub(crate) fn service_due_scroll_motion(&mut self, now: std::time::Instant) -> Vec<WindowId> {
        let mut ticked: Vec<WindowId> = Vec::new();
        let mut glide_due: Vec<WindowId> = Vec::new();
        let mut bounce_due: Vec<WindowId> = Vec::new();
        for (&id, ws) in &self.windows {
            if ws
                .scroll_glide
                .as_ref()
                .is_some_and(|st| now >= st.wake_deadline())
            {
                glide_due.push(id);
            }
            if ws
                .overscroll
                .as_ref()
                .is_some_and(|sp| now >= sp.wake_deadline())
            {
                bounce_due.push(id);
            }
        }
        for id in glide_due {
            self.tick_scroll_glide(id, now);
            if let Some(ws) = self.windows.get_mut(&id)
                && let Some(sp) = ws.overscroll.as_ref()
            {
                ws.scroll_frac_px = sp.sample(now).0.round() as i32;
            }
            ticked.push(id);
            if !bounce_due.contains(&id)
                && self
                    .windows
                    .get(&id)
                    .and_then(|ws| ws.overscroll.as_ref())
                    .is_some_and(|sp| now >= sp.wake_deadline())
            {
                bounce_due.push(id);
            }
        }
        for id in bounce_due {
            if self
                .windows
                .get(&id)
                .and_then(|ws| ws.overscroll.as_ref())
                .is_some_and(|sp| now >= sp.wake_deadline())
            {
                self.tick_overscroll(id, now);
                if !ticked.contains(&id) {
                    ticked.push(id);
                }
            }
        }
        ticked
    }

    /// One deadline wake of an in-flight elastic-overscroll bounce.
    pub(crate) fn tick_overscroll(&mut self, wid: WindowId, now: std::time::Instant) {
        let animate = self.smooth_scroll_animates(wid);
        let app_interval = self.frame_interval;
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let Some((disp, done)) = ws.overscroll.as_ref().map(|sp| sp.sample(now)) else {
            return;
        };
        if !animate {
            ws.overscroll = None;
            ws.scroll_frac_px = 0;
            if let Some(w) = ws.os_window.as_ref() {
                w.request_redraw();
            }
            return;
        }
        ws.scroll_frac_px = disp.round() as i32;
        let tick_interval = ws.frame_interval.unwrap_or(app_interval);
        if let Some(sp) = ws.overscroll.as_mut() {
            let next = crate::app_render::phase_locked_effect_deadline(
                now,
                tick_interval,
                Some(sp.next_tick()),
                None,
                true,
            );
            sp.set_next_tick(next);
        }
        if done {
            ws.overscroll = None;
            ws.scroll_frac_px = 0;
        }
        ws.scroll_pill.touch(now);
        if let Some(w) = ws.os_window.as_ref() {
            w.request_redraw();
        }
    }

    /// The `InputEvent::ScrollView` arm of [`App::input`].
    fn input_scroll_view(
        &mut self,
        wid: WindowId,
        intent: ScrollIntent,
        term: &Arc<Mutex<Terminal>>,
    ) -> InputOutcome {
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.fx
                .cursor_mut()
                .glow
                .clear_typed(std::time::Instant::now());
            ws.fx.cursor_mut().trail.clear_typed();
        }
        if let Some(rain) = self.windows.get_mut(&wid).and_then(|ws| ws.fx.rain_mut())
            && term_lock(term).is_alternate_screen()
        {
            rain.note_alt_scroll();
        }
        {
            let mut term = term_lock(term);
            let page = i32::from(term.rows()).max(1);
            match intent {
                ScrollIntent::Up => term.scroll_display(page),
                ScrollIntent::Down => term.scroll_display(-page),
                ScrollIntent::By(n) => term.scroll_display(n),
                ScrollIntent::Top => term.scroll_to_top(),
                ScrollIntent::Bottom => term.return_to_live(),
                ScrollIntent::PrevPrompt | ScrollIntent::NextPrompt => {
                    if let Some(row) = crate::input::jump_prompt_target(
                        &term,
                        matches!(intent, ScrollIntent::PrevPrompt),
                    ) {
                        term.scroll_to_absolute_row(row);
                    }
                }
            }
        }
        self.drop_front_scroll_motion(wid);
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.scroll_pill.touch(std::time::Instant::now());
        }
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
        InputOutcome::Ok
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the paste lane carries the session's four capability handles (terminal, \
                  mode mirror, sink, echo tracker) plus the injected clock explicitly, like \
                  its mouse/wheel siblings — bundling them would add a per-event struct"
    )]
    fn input_paste(
        &mut self,
        wid: WindowId,
        session: u64,
        ev: InputEvent,
        term: &Arc<Mutex<Terminal>>,
        modes: &Arc<ModeMirror>,
        sink: &Arc<SinkWriter>,
        echo: &Arc<OutputEchoTracker>,
        input_now: std::time::Instant,
        completion: Option<crate::control_input_ack::InputCompletion>,
    ) -> InputOutcome {
        debug_assert!(matches!(&ev, InputEvent::Paste(..)));
        let movement_capable = matches!(&ev, InputEvent::Paste(text, _)
            if aterm_core::terminal::Terminal::paste_has_payload(text));
        let erase_zeros = match &ev {
            InputEvent::Paste(text, _) if movement_capable => aterm_grapheme::split_graphemes(text)
                .take(ErasePriceMemory::CAP)
                .count(),
            _ => 0,
        };
        let insert = match &ev {
            InputEvent::Paste(text, _) if movement_capable => {
                let ambiguous_width_double = term_lock(term).modes().ambiguous_width_double;
                Some(paste_insert_width(text, ambiguous_width_double))
            }
            _ => None,
        };
        let tty_swallows = movement_capable && sink.tty_swallows_input();
        let ticket = insert
            .filter(|width| *width != InsertWidth::Cells(0))
            .filter(|_| !tty_swallows)
            .map(|width| DeliveryTicket {
                insert: Some(width),
                window: None,
                key: None,
                dispatched_at: input_now,
            });
        let paste_bytes = match &ev {
            InputEvent::Paste(text, _) => text.len() as u64,
            _ => 0,
        };
        let meter = aterm_messages::waits::paste_is_watched(paste_bytes)
            .then(|| Arc::new(aterm_session::sink::BulkMeter::new()));
        match paste_order::enqueue_metered_observed(
            term,
            modes,
            sink,
            echo,
            ev,
            ticket,
            None,
            meter.clone(),
            completion,
        ) {
            Ok(()) => {
                if let Some(meter) = meter {
                    self.watch_paste(session, sink, meter, paste_bytes, input_now);
                }
            }
            Err(rejected) => {
                self.refuse_input(session, InputRefusal::of_rejected(rejected));
                return rejected_outcome(rejected);
            }
        }
        self.snap_to_bottom(wid);
        if movement_capable && let Some(ws) = self.windows.get_mut(&wid) {
            ws.fx.cursor_mut().glow.note_user_gesture(input_now);
            ws.fx.cursor_mut().trail.note_user_gesture(input_now);
            ws.erase_prices.push_zeros(erase_zeros);
            if ws.fx.cursor_mut().glow.cue_paste(input_now)
                && let Some(w) = ws.os_window.as_ref()
            {
                w.request_redraw();
            }
        }
        if movement_capable && let Some(ws) = self.windows.get_mut(&wid) {
            revoke_dispatch(ws, input_now, false);
            ws.fx
                .companion_mut()
                .note_console_input(input_now, PetInputKind::Paste);
        }
        InputOutcome::Ok
    }

    /// The `InputEvent::Resize` arm of [`App::input`] (range-reject reportable).
    fn input_resize(
        &mut self,
        wid: WindowId,
        rows: u16,
        cols: u16,
        echo_to_window: bool,
    ) -> InputOutcome {
        if !(1..=aterm_core::grid::MAX_GRID_ROWS).contains(&rows)
            || !(1..=aterm_core::grid::MAX_GRID_COLS).contains(&cols)
        {
            return InputOutcome::RangeRejected;
        }
        if echo_to_window {
            self.apply_grid_resize(rows, cols);
        } else {
            self.apply_term_resize(wid, rows, cols);
        }
        let target = if echo_to_window {
            self.frontmost_window.unwrap_or(wid)
        } else {
            wid
        };
        let _ = self.reconcile_active_editor_viewport(target);
        InputOutcome::Ok
    }

    /// The [`InputEvent::ResizeWindowPx`] arm of [`App::input`].
    fn input_resize_window_px(&mut self, wid: WindowId, width: u32, height: u32) -> InputOutcome {
        if width == 0 || height == 0 {
            return InputOutcome::RangeRejected;
        }
        let target = self.frontmost_window.unwrap_or(wid);
        #[cfg(target_os = "linux")]
        if let Some(ws) = self.windows.get_mut(&target) {
            ws.initial_frame_settle = None;
        }
        if let Some(w) = self
            .windows
            .get(&target)
            .and_then(|ws| ws.os_window.clone())
        {
            // Best-effort: the WM may clamp or ignore it, so no local geometry is
            // written on our own request, except when the backend answers `Some`
            // (Wayland applies synchronously and sends no `Resized`): that answer is
            // fed through the event's path.
            if let Some(applied) =
                w.request_inner_size(aterm_window::dpi::PhysicalSize::new(width, height))
            {
                crate::metrics::note_resize_arrival();
                if let Some(ws) = self.windows.get_mut(&target) {
                    ws.win_px = Some(applied);
                    ws.last_resized_event_at = Some(std::time::Instant::now());
                }
                self.sync_surface_to_window(target);
                self.on_resize(target, applied);
                w.request_redraw();
            }
        }
        InputOutcome::Ok
    }

    /// Seam-internal left-press gesture dispatch for both sources.
    pub(crate) fn seam_left_press(
        &mut self,
        wid: WindowId,
        row: u16,
        col: u16,
        click_count: u8,
        block: bool,
    ) {
        let Some(term) = self
            .front_terminal(wid)
            .map(|terminal| terminal.term.clone())
        else {
            return;
        };
        let (sel_row, col) = {
            let term = term_lock(&term);
            let sel_row = i32::from(row) - term.grid().display_offset() as i32;
            (sel_row, crate::app_mouse::pointer_char(&term, row, col).0)
        };
        match click_count {
            2 => self.select_word_click(wid, sel_row, col),
            3 => self.select_line_click(wid, sel_row, col),
            _ => self.begin_selection(
                wid,
                if block {
                    SelectionType::Block
                } else {
                    SelectionType::Simple
                },
            ),
        }
    }

    /// Records that this key's press was consumed by a GUI gate (no PTY press report).
    fn note_consumed_press(&mut self, wid: WindowId, ev: &KeyEvent) {
        self.note_consumed_press_key(wid, ev.physical_key, ev.repeat);
    }

    /// The field-level core of [`note_consumed_press`], split out for unit tests.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "input_release_pairing",
            action = "ConsumePhysicalPress",
            project = "aterm_gui::app_input::input_release_pairing_conformance::project"
        )
    )]
    fn note_consumed_press_key(
        &mut self,
        wid: WindowId,
        physical_key: aterm_window::keyboard::PhysicalKey,
        repeat: bool,
    ) {
        // Distinct branch.
        if repeat {
            // Distinct branch.
            return;
        }
        self.retire_previous_physical_press_owner(wid, physical_key);
        self.physical_press_owners.insert(
            physical_key,
            crate::PhysicalPressOwner::Consumed { window: wid },
        );
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.consumed_press_keys.insert(physical_key);
        }
    }

    /// Record the exact terminal/key identity that received a physical press.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "input_release_pairing",
            action = "ForwardPress",
            project = "aterm_gui::app_input::input_release_pairing_conformance::project"
        )
    )]
    #[allow(
        clippy::too_many_arguments,
        reason = "the press record is the whole InputEvent::Key identity plus its route"
    )]
    fn note_forwarded_press_key(
        &mut self,
        wid: WindowId,
        physical_key: aterm_window::keyboard::PhysicalKey,
        repeat: bool,
        key: aterm_types::keyboard::Key,
        mods: aterm_types::keyboard::Modifiers,
        base_layout: Option<char>,
        layout_text: Option<char>,
    ) {
        if repeat {
            return;
        }
        let Some(session) = self.front_terminal(wid).map(|terminal| terminal.session) else {
            self.note_consumed_press_key(wid, physical_key, false);
            return;
        };
        self.retire_previous_physical_press_owner(wid, physical_key);
        self.physical_press_owners.insert(
            physical_key,
            crate::PhysicalPressOwner::Forwarded {
                window: wid,
                session,
                key,
                mods,
                base_layout,
                layout_text,
            },
        );
    }

    /// Record and deliver literal text/raw bytes as one immutable physical-key episode.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "input_release_pairing",
            action = "ForwardLiteralPress",
            project = "aterm_gui::app_input::input_release_pairing_conformance::project"
        )
    )]
    fn forward_literal_press(
        &mut self,
        wid: WindowId,
        physical_key: aterm_window::keyboard::PhysicalKey,
        event: InputEvent,
        repeat: Option<InputEvent>,
    ) {
        debug_assert!(matches!(
            event,
            InputEvent::Text(_) | InputEvent::KeySequence(_)
        ));
        let Some(session) = self.front_terminal(wid).map(|terminal| terminal.session) else {
            self.note_consumed_press_key(wid, physical_key, false);
            return;
        };
        self.retire_previous_physical_press_owner(wid, physical_key);
        self.physical_press_owners.insert(
            physical_key,
            crate::PhysicalPressOwner::Literal {
                window: wid,
                session,
                event: event.clone(),
                repeat,
            },
        );
        #[cfg(test)]
        clear_physical_release_trace();
        #[cfg(test)]
        let trace_event = event.clone();
        let outcome = self.input_to_session(
            wid,
            event,
            Source::Human,
            Some(session),
            crate::app_input::PressPhase::Initial,
        );
        #[cfg(test)]
        record_physical_release_trace(PhysicalReleaseTrace::Literal {
            arrival_window: wid,
            press_window: wid,
            session,
            event: trace_event,
            repeated: false,
            delivery: match outcome {
                InputOutcome::Ok => input::Delivery::Full,
                InputOutcome::WriteFailed | InputOutcome::QueueFull => input::Delivery::Failed,
                InputOutcome::RangeRejected => {
                    unreachable!("a raw key sequence cannot produce a resize range verdict")
                }
            },
        });
        #[cfg(not(test))]
        let _ = outcome;
    }

    /// Repeat the literal payload captured by [`forward_literal_press`].
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "input_release_pairing",
            action = "ForwardRepeatOfLiteralPress",
            project = "aterm_gui::app_input::input_release_pairing_conformance::project"
        )
    )]
    fn repeat_literal(
        &mut self,
        repeat_window: WindowId,
        physical_key: aterm_window::keyboard::PhysicalKey,
    ) -> bool {
        #[cfg(test)]
        clear_physical_release_trace();
        let Some(crate::PhysicalPressOwner::Literal {
            window,
            session,
            event,
            repeat,
        }) = self.physical_press_owners.get(&physical_key)
        else {
            return false;
        };
        let (window, session, event) =
            (*window, *session, repeat.as_ref().unwrap_or(event).clone());
        if self.pool.get(session).is_none() {
            return true;
        }
        #[cfg(test)]
        let trace_event = event.clone();
        let outcome = self.input_to_session(
            window,
            event,
            Source::Human,
            Some(session),
            PressPhase::Repeat,
        );
        #[cfg(test)]
        record_physical_release_trace(PhysicalReleaseTrace::Literal {
            arrival_window: repeat_window,
            press_window: window,
            session,
            event: trace_event,
            repeated: true,
            delivery: match outcome {
                InputOutcome::Ok => input::Delivery::Full,
                InputOutcome::WriteFailed | InputOutcome::QueueFull => input::Delivery::Failed,
                InputOutcome::RangeRejected => {
                    unreachable!("a raw key sequence cannot produce a resize range verdict")
                }
            },
        });
        #[cfg(not(test))]
        let _ = (repeat_window, outcome);
        true
    }

    /// Capture one repeatable GUI/native action after its genuine press was classified.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "input_release_pairing",
            action = "CaptureLocalRepeatPress",
            project = "aterm_gui::app_input::input_release_pairing_conformance::project"
        )
    )]
    fn note_local_repeat_press(
        &mut self,
        wid: WindowId,
        physical_key: aterm_window::keyboard::PhysicalKey,
        action: crate::LocalRepeatAction,
    ) {
        self.retire_previous_physical_press_owner(wid, physical_key);
        self.physical_press_owners.insert(
            physical_key,
            crate::PhysicalPressOwner::Local {
                window: wid,
                action,
            },
        );
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.consumed_press_keys.insert(physical_key);
        }
    }

    /// One press-disposition tail for [`Self::on_key`]'s gate chain.
    fn note_press_disposition(
        &mut self,
        wid: WindowId,
        ev: &KeyEvent,
        repeat: Option<crate::LocalRepeatAction>,
    ) {
        match repeat {
            Some(action) => self.note_local_repeat_press(wid, ev.physical_key, action),
            None => self.note_consumed_press(wid, ev),
        }
    }

    /// Apply one captured local repeat only while its original owner identity remains live.
    fn apply_local_repeat_action(
        &mut self,
        wid: WindowId,
        action: crate::LocalRepeatAction,
    ) -> bool {
        use crate::{FontZoomRepeatAction, LocalRepeatAction};

        match action {
            LocalRepeatAction::Native { view, mut event } => {
                if self.active_native_view(wid).map(|(_, active)| active) != Some(view) {
                    return false;
                }
                if let InputEvent::Key { event_type, .. } = &mut event {
                    *event_type = aterm_types::keyboard::KeyEventType::Repeat;
                }
                self.note_update_handoff_activity();
                self.reset_blink(wid);
                let arrival = crate::metrics::native_input_arrival();
                self.timed_native_input_event(wid, &event, Some(arrival))
            }
            LocalRepeatAction::Search { session, action } => {
                if self.front_terminal(wid).map(|terminal| terminal.session) != Some(session)
                    || self.windows.get(&wid).is_none_or(|ws| ws.search.is_none())
                {
                    return false;
                }
                self.note_update_handoff_activity();
                self.reset_blink(wid);
                self.apply_search_repeat_action(wid, action);
                true
            }
            LocalRepeatAction::Rename { session, edit } => {
                // The owner is the session being renamed, checked against the live
                // in-grid edit: a repeat outliving its editor is dropped.
                if self.inline_rename_edit(wid).map(|live| live.session) != Some(session) {
                    return false;
                }
                self.note_update_handoff_activity();
                self.reset_blink(wid);
                self.rename_field_edit(wid, edit)
            }
            LocalRepeatAction::Scroll { session, intent } => {
                if self.front_terminal(wid).map(|terminal| terminal.session) != Some(session) {
                    return false;
                }
                let _ = self.input_to_session(
                    wid,
                    InputEvent::ScrollView(intent),
                    Source::Human,
                    Some(session),
                    PressPhase::Repeat,
                );
                true
            }
            LocalRepeatAction::FontZoom(action) => {
                if !self.windows.contains_key(&wid) {
                    return false;
                }
                self.note_update_handoff_activity();
                match action {
                    FontZoomRepeatAction::Increase => {
                        self.set_font_size(self.font_size + FONT_ZOOM_STEP);
                    }
                    FontZoomRepeatAction::Decrease => {
                        self.set_font_size(self.font_size - FONT_ZOOM_STEP);
                    }
                    FontZoomRepeatAction::Reset => self.set_font_size(self.default_font_size),
                }
                true
            }
            LocalRepeatAction::Palette(key) => {
                if self
                    .windows
                    .get(&wid)
                    .and_then(|ws| ws.overlay.as_ref())
                    .map(|overlay| overlay.kind())
                    != Some(crate::overlay::OverlayKind::Palette)
                {
                    return false;
                }
                self.note_update_handoff_activity();
                self.reset_blink(wid);
                self.apply_palette_key(key);
                true
            }
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "input_release_pairing",
            action = "ForwardLocalRepeat",
            project = "aterm_gui::app_input::input_release_pairing_conformance::project"
        )
    )]
    fn repeat_local_press(
        &mut self,
        arrival_window: WindowId,
        physical_key: aterm_window::keyboard::PhysicalKey,
    ) -> bool {
        #[cfg(test)]
        clear_physical_release_trace();
        let Some(crate::PhysicalPressOwner::Local { window, action }) =
            self.physical_press_owners.get(&physical_key)
        else {
            return false;
        };
        let (window, action) = (*window, action.clone());
        if self.apply_local_repeat_action(window, action) {
            #[cfg(test)]
            record_physical_release_trace(PhysicalReleaseTrace::Local {
                arrival_window,
                press_window: window,
            });
        }
        #[cfg(not(test))]
        let _ = arrival_window;
        true
    }

    /// Closes a stale press episode before a fresh non-repeat press takes its physical key.
    fn retire_previous_physical_press_owner(
        &mut self,
        replacement_window: WindowId,
        physical_key: aterm_window::keyboard::PhysicalKey,
    ) {
        if self.physical_press_owners.contains_key(&physical_key) {
            self.release_physical_press(replacement_window, physical_key);
        }
    }

    /// Whether `physical_key`'s press is tracked as GUI-consumed.
    fn press_was_consumed(
        &self,
        _wid: WindowId,
        physical_key: aterm_window::keyboard::PhysicalKey,
    ) -> bool {
        matches!(
            self.physical_press_owners.get(&physical_key),
            Some(crate::PhysicalPressOwner::Consumed { .. })
        )
    }

    /// A repeat without an observed physical press (a focus epoch began mid-hold or the key-down
    /// was lost) has no protocol peer or destination.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "input_release_pairing",
            action = "SwallowUntrackedRepeat",
            project = "aterm_gui::app_input::input_release_pairing_conformance::project"
        )
    )]
    fn repeat_is_untracked(&self, physical_key: aterm_window::keyboard::PhysicalKey) -> bool {
        !self.physical_press_owners.contains_key(&physical_key)
    }

    /// Resolves an OS auto-repeat from press-time physical ownership ([`Self::on_key`]).
    fn route_physical_repeat(
        &mut self,
        arrival_window: WindowId,
        physical_key: aterm_window::keyboard::PhysicalKey,
    ) {
        if self.repeat_local_press(arrival_window, physical_key) {
            return;
        }
        if self.press_was_consumed(arrival_window, physical_key) {
            return;
        }
        if self.repeat_literal(arrival_window, physical_key) {
            return;
        }
        if self.repeat_forwarded_press(arrival_window, physical_key) {
            return;
        }
        if self.repeat_is_untracked(physical_key) {
            return;
        }
        unreachable!("every physical press-owner variant was handled above");
    }

    /// Forwards an auto-repeat to the owner established by the physical press, before live GUI and
    /// content gates, so a focus.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "input_release_pairing",
            action = "ForwardRepeatOfForwardedPress",
            project = "aterm_gui::app_input::input_release_pairing_conformance::project"
        )
    )]
    fn repeat_forwarded_press(
        &mut self,
        repeat_window: WindowId,
        physical_key: aterm_window::keyboard::PhysicalKey,
    ) -> bool {
        #[cfg(test)]
        clear_physical_release_trace();
        let Some(crate::PhysicalPressOwner::Forwarded {
            window,
            session,
            key,
            mods,
            base_layout,
            layout_text,
        }) = self.physical_press_owners.get(&physical_key)
        else {
            return false;
        };
        let (window, session, key, mods, base_layout, layout_text) = (
            *window,
            *session,
            key.clone(),
            *mods,
            *base_layout,
            *layout_text,
        );
        if self.pool.get(session).is_none() {
            return true;
        }

        #[cfg(test)]
        let trace_key = key.clone();
        let outcome = self.input_to_session(
            window,
            InputEvent::Key {
                key,
                mods,
                base_layout,
                layout_text,
                event_type: aterm_types::keyboard::KeyEventType::Repeat,
            },
            Source::Human,
            Some(session),
            PressPhase::Repeat,
        );
        #[cfg(test)]
        {
            let delivery = match outcome {
                InputOutcome::Ok => input::Delivery::Full,
                InputOutcome::WriteFailed | InputOutcome::QueueFull => input::Delivery::Failed,
                InputOutcome::RangeRejected => {
                    unreachable!("a key repeat cannot produce a resize range verdict")
                }
            };
            record_physical_release_trace(PhysicalReleaseTrace::Forwarded {
                arrival_window: repeat_window,
                press_window: window,
                session,
                key: trace_key,
                mods,
                base_layout,
                event_type: aterm_types::keyboard::KeyEventType::Repeat,
                delivery,
            });
        }
        #[cfg(not(test))]
        let _ = (repeat_window, outcome);
        true
    }

    /// If `physical_key`'s press was recorded as GUI-consumed ([`note_consumed_press`]), removes it
    /// and returns `true`.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "input_release_pairing",
            action = "ReleaseConsumedPress",
            project = "aterm_gui::app_input::input_release_pairing_conformance::project"
        )
    )]
    fn take_consumed_release(
        &mut self,
        release_window: WindowId,
        physical_key: aterm_window::keyboard::PhysicalKey,
    ) -> bool {
        #[cfg(not(test))]
        let _ = release_window;
        let Some(&crate::PhysicalPressOwner::Consumed { window }) =
            self.physical_press_owners.get(&physical_key)
        else {
            return false;
        };
        self.physical_press_owners.remove(&physical_key);
        if let Some(ws) = self.windows.get_mut(&window) {
            ws.consumed_press_keys.remove(&physical_key);
        }
        #[cfg(test)]
        record_physical_release_trace(PhysicalReleaseTrace::Consumed {
            arrival_window: release_window,
            press_window: window,
        });
        true
    }

    /// Retire a literal-sequence episode without fabricating a protocol release.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "input_release_pairing",
            action = "ReleaseLiteralPress",
            project = "aterm_gui::app_input::input_release_pairing_conformance::project"
        )
    )]
    fn take_literal_release(
        &mut self,
        release_window: WindowId,
        physical_key: aterm_window::keyboard::PhysicalKey,
    ) -> bool {
        #[cfg(not(test))]
        let _ = release_window;
        let Some(&crate::PhysicalPressOwner::Literal { window, .. }) =
            self.physical_press_owners.get(&physical_key)
        else {
            return false;
        };
        #[cfg(not(test))]
        let _ = window;
        self.physical_press_owners.remove(&physical_key);
        #[cfg(test)]
        record_physical_release_trace(PhysicalReleaseTrace::Consumed {
            arrival_window: release_window,
            press_window: window,
        });
        true
    }

    /// Retire a repeatable GUI/native episode.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "input_release_pairing",
            action = "ReleaseLocalRepeatPress",
            project = "aterm_gui::app_input::input_release_pairing_conformance::project"
        )
    )]
    fn take_local_repeat_release(
        &mut self,
        release_window: WindowId,
        physical_key: aterm_window::keyboard::PhysicalKey,
    ) -> bool {
        #[cfg(not(test))]
        let _ = release_window;
        let Some(&crate::PhysicalPressOwner::Local { window, .. }) =
            self.physical_press_owners.get(&physical_key)
        else {
            return false;
        };
        self.physical_press_owners.remove(&physical_key);
        if let Some(ws) = self.windows.get_mut(&window) {
            ws.consumed_press_keys.remove(&physical_key);
        }
        #[cfg(test)]
        record_physical_release_trace(PhysicalReleaseTrace::Consumed {
            arrival_window: release_window,
            press_window: window,
        });
        true
    }

    /// Finishes one physical hold by its press-time disposition.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "input_release_pairing",
            action = "ReleaseForwardedPress",
            project = "aterm_gui::app_input::input_release_pairing_conformance::project"
        )
    )]
    pub(crate) fn release_physical_press(
        &mut self,
        release_window: WindowId,
        physical_key: aterm_window::keyboard::PhysicalKey,
    ) {
        #[cfg(test)]
        clear_physical_release_trace();
        if self.take_consumed_release(release_window, physical_key) {
            return;
        }
        if self.take_literal_release(release_window, physical_key) {
            return;
        }
        if self.take_local_repeat_release(release_window, physical_key) {
            return;
        }
        let Some(owner) = self.physical_press_owners.remove(&physical_key) else {
            return;
        };
        match owner {
            crate::PhysicalPressOwner::Consumed { .. } => {
                unreachable!("consumed physical owner must be removed by take_consumed_release")
            }
            crate::PhysicalPressOwner::Forwarded {
                window,
                session,
                key,
                mut mods,
                base_layout,
                layout_text,
            } => {
                // A modifier's release reports the state it leaves (kitty): its
                // own bit stays set while the other side is still held (both
                // Shifts down, one let go) and is clear otherwise.
                if let aterm_types::keyboard::Key::Named(named) = &key
                    && let Some((flag, _)) = named.modifier_twin()
                {
                    let twin_held = aterm_winit_keymap::physical_modifier_twin(physical_key)
                        .is_some_and(|twin| self.physical_press_owners.contains_key(&twin));
                    mods.set(flag, twin_held);
                }
                self.note_update_handoff_tolerated_activity();
                #[cfg(test)]
                let trace_key = key.clone();
                #[cfg(not(test))]
                let _ = window;
                let byte_silent = sampled_release_relevance(session) == Some(false);
                let Some(session) = self.pool.get(session) else {
                    return;
                };
                let event = InputEvent::Key {
                    key,
                    mods,
                    base_layout,
                    layout_text,
                    event_type: aterm_types::keyboard::KeyEventType::Release,
                };
                let receipt = (!byte_silent).then(|| {
                    paste_order::ordered_or_inline(
                        &session.term,
                        &session.ctx.modes,
                        &session.ctx.sink,
                        &session.ctx.output_echo,
                        &event,
                        None,
                        None,
                    )
                    .0
                });
                #[cfg(test)]
                {
                    let delivery = match receipt.map(|receipt| receipt.egress) {
                        None => input::Delivery::Full,
                        Some(input::Egress::Reported(delivery)) => delivery,
                        Some(_) => unreachable!("a key release always returns Reported"),
                    };
                    record_physical_release_trace(PhysicalReleaseTrace::Forwarded {
                        arrival_window: release_window,
                        press_window: window,
                        session: session.id,
                        key: trace_key,
                        mods,
                        base_layout,
                        event_type: aterm_types::keyboard::KeyEventType::Release,
                        delivery,
                    });
                }
                let session_id = session.id;
                if receipt.is_some_and(input::EgressReceipt::input_queue_full) {
                    self.refuse_input(session_id, InputRefusal::Busy);
                }
            }
            crate::PhysicalPressOwner::Literal { .. } => {
                unreachable!("literal physical owner must be removed by take_literal_release")
            }
            crate::PhysicalPressOwner::Local { .. } => {
                unreachable!("local physical owner must be removed by take_local_repeat_release")
            }
        }
    }

    /// Routes one physical key press while a native view owns the window.
    fn on_key_native_mode(&mut self, wid: WindowId, mods: ModifiersState, ev: &KeyEvent) -> bool {
        let Some((_, native_view)) = self.active_native_view(wid) else {
            return false;
        };

        if !self.keybindings.is_empty() || !self.key_sequences.is_empty() {
            let base = base_logical_key(ev);
            match keybinding::resolve_chord(
                &base,
                ev.physical_key,
                mods,
                &self.keybindings,
                &self.key_sequences,
                |dir| self.focus_pane_would_move(dir),
            ) {
                keybinding::ChordResolution::Action(action) => {
                    if action == keybinding::Action::Copy {
                        let _ = self.copy_native_selection(wid);
                    } else if action == keybinding::Action::SelectAll {
                        let _ = self.dispatch_native_event(
                            wid,
                            crate::native_app::AppEvent::TextInput(
                                crate::native_app::TextInputEvent::SelectAll,
                            ),
                        );
                    } else if native_binding_allowed(action) {
                        self.dispatch_action(wid, action);
                    }
                    self.note_consumed_press(wid, ev);
                    return true;
                }
                keybinding::ChordResolution::Sequence(_) => {
                    self.note_consumed_press(wid, ev);
                    return true;
                }
                keybinding::ChordResolution::FallThrough => {}
            }
        }

        if HARDCODED_SUPER_CHORDS
            && (self.on_key_super_shift_chord(mods, ev)
                || self.on_key_super_chord(mods, ev)
                || self.on_key_pane_focus(mods, ev))
        {
            self.note_consumed_press(wid, ev);
            return true;
        }

        if HARDCODED_SUPER_CHORDS
            && mods.super_key()
            && let Key::Character(character) = &ev.logical_key
        {
            if character.eq_ignore_ascii_case("f") {
                self.find_requested_in(wid);
                self.note_consumed_press(wid, ev);
                return true;
            }
            if character.eq_ignore_ascii_case("c") {
                let _ = self.copy_native_selection(wid);
                self.note_consumed_press(wid, ev);
                return true;
            }
            if character.eq_ignore_ascii_case("v") {
                self.paste_clipboard();
                self.note_consumed_press(wid, ev);
                return true;
            }
        }

        if self.native_preedit_active(wid) {
            self.note_consumed_press(wid, ev);
            return true;
        }

        let km_mods = keymap::modifiers_from_winit(mods);
        let option_composes = self.option_composes_in(wid);
        if let Some((key, km_mods, base_layout, layout_text)) =
            keymap::build_key_input(ev, km_mods, option_composes)
        {
            let input = InputEvent::Key {
                key,
                mods: km_mods,
                base_layout,
                layout_text,
                event_type: aterm_types::keyboard::KeyEventType::Press,
            };
            self.note_local_repeat_press(
                wid,
                ev.physical_key,
                crate::LocalRepeatAction::Native {
                    view: native_view,
                    event: input.clone(),
                },
            );
            self.input(wid, input, Source::Human);
            return true;
        }

        let bare = !mods.control_key() && !mods.alt_key() && !mods.super_key();
        if let Some(text) = &ev.text
            && bare
            && !text.is_empty()
        {
            let input = InputEvent::Text(text.to_string());
            self.note_local_repeat_press(
                wid,
                ev.physical_key,
                crate::LocalRepeatAction::Native {
                    view: native_view,
                    event: input.clone(),
                },
            );
            self.input(wid, input, Source::Human);
        } else {
            self.note_consumed_press(wid, ev);
        }
        true
    }

    fn native_preedit_active(&self, wid: WindowId) -> bool {
        let Some((instance, view)) = self.active_native_view(wid) else {
            return false;
        };
        if self
            .windows
            .get(&wid)
            .is_some_and(|window| !window.preedit.is_empty())
        {
            return true;
        }
        match self.native_runtime.view_state(view) {
            Some(crate::native_app::AppViewState::Editor(state)) => !state.preedit.is_empty(),
            Some(crate::native_app::AppViewState::Settings(state)) => state
                .editing_field
                .as_ref()
                .and_then(|key| state.field_inputs.get(key))
                .unwrap_or(&state.search_input)
                .preedit()
                .is_some(),
            Some(crate::native_app::AppViewState::SystemState(state)) => {
                let Some(crate::native_app::NativeApp::SystemState(app)) =
                    self.native_runtime.app(instance)
                else {
                    return false;
                };
                state
                    .common
                    .last_focus
                    .as_ref()
                    .and_then(|key| app.field_projection(key.as_str()))
                    .is_some_and(|field| field.preedit.is_some())
            }
            Some(
                crate::native_app::AppViewState::Markdown(_)
                | crate::native_app::AppViewState::Recovery(_),
            )
            | None => false,
        }
    }

    /// Resolve only text selected by the active native view.
    pub(crate) fn native_selection_text(&self, wid: WindowId) -> Option<String> {
        let (instance, view) = self.active_native_view(wid)?;
        let text = match self.native_runtime.view_state(view) {
            Some(crate::native_app::AppViewState::Editor(state)) => {
                let buffer = state.buffer.as_ref()?;
                let snapshot = self.document_store.snapshot(buffer.document)?;
                let selected = buffer
                    .selections
                    .iter()
                    .filter_map(|selection| {
                        let range = selection.range();
                        (!range.is_empty()
                            && range.end <= snapshot.text.len()
                            && snapshot.text.is_char_boundary(range.start)
                            && snapshot.text.is_char_boundary(range.end))
                        .then(|| snapshot.text[range].to_string())
                    })
                    .collect::<Vec<_>>();
                selected.join("\n")
            }
            Some(crate::native_app::AppViewState::Markdown(state)) => {
                let range = state.selection.clone()?;
                let document = self.native_runtime.document_id(instance)?;
                let snapshot = self.document_store.snapshot(document)?;
                if range.end > snapshot.text.len()
                    || !snapshot.text.is_char_boundary(range.start)
                    || !snapshot.text.is_char_boundary(range.end)
                {
                    return None;
                }
                snapshot.text[range].to_string()
            }
            Some(crate::native_app::AppViewState::Settings(state)) => {
                let input = state
                    .editing_field
                    .as_ref()
                    .and_then(|key| state.field_inputs.get(key))
                    .unwrap_or(&state.search_input);
                input.selected_text().to_string()
            }
            Some(crate::native_app::AppViewState::SystemState(state)) => {
                let crate::native_app::NativeApp::SystemState(app) =
                    self.native_runtime.app(instance)?
                else {
                    return None;
                };
                let key = state.common.last_focus.as_ref()?;
                app.text_input(key.as_str())?.selected_text().to_string()
            }
            Some(crate::native_app::AppViewState::Recovery(_)) => return None,
            None => return None,
        };
        (!text.is_empty()).then_some(text)
    }

    /// Copy only text selected by the active native view.
    fn copy_native_selection(&self, wid: WindowId) -> bool {
        self.native_selection_text(wid)
            .is_some_and(|text| crate::control::pbcopy(&text))
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "FocusModifierCache",
            action = "PressL",
            project = "aterm_gui::multi_window_tests::project_focus_modifier_cache"
        )
    )]
    pub(crate) fn on_key(&mut self, wid: WindowId, ev: KeyEvent) {
        if ev.state != ElementState::Pressed {
            self.release_physical_press(wid, ev.physical_key);
            return;
        }
        let mods = self.windows.get(&wid).map(|ws| ws.mods);
        let option_composes = self.option_composes_in(wid);
        let (ev, mods) = self.option_as_layout_level(ev, mods, option_composes);
        let press = mods.and_then(|m| physical_press(&ev, m, option_composes));
        let inert = keymap::press_is_inert(&ev);
        let mut base_key: Option<Key> = None;
        let fence = match &press {
            Some(press) => press_fence(press),
            None if inert => None,
            None => Some(false),
        };
        match fence {
            Some(false) if mods.is_some_and(|m| self.press_pastes(m, &ev, &mut base_key)) => {
                self.clear_move_license_keeping_hand(wid);
            }
            Some(keep_hand) => self.fence_move_license(wid, keep_hand),
            None if !inert => {
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.fx.cursor_mut().glow.supersede_typed_press();
                }
            }
            None => {}
        }
        if ev.repeat {
            self.route_physical_repeat(wid, ev.physical_key);
            return;
        }
        let Some(mods) = mods else {
            return;
        };
        if !inert {
            self.reset_blink(wid);
        }
        if self.paste_banner.as_ref().is_some_and(|p| p.wid == wid) {
            let (chars, code) = match &ev.logical_key {
                Key::Named(NamedKey::Enter) => (Some("\r"), 0),
                Key::Named(NamedKey::Escape) => (Some("\u{1b}"), 0),
                _ => (None, 0),
            };
            match crate::alert_keys::confirm_key(true, 0, chars, code) {
                crate::alert_keys::ConfirmKey::Accept => self.answer_paste_banner(true),
                crate::alert_keys::ConfirmKey::Cancel => self.answer_paste_banner(false),
                crate::alert_keys::ConfirmKey::PassThrough => {}
            }
            self.note_press_disposition(wid, &ev, None);
            return;
        }
        if self.close_banner.as_ref().is_some_and(|p| p.wid == wid) {
            let (chars, code) = match &ev.logical_key {
                Key::Named(NamedKey::Enter) => (Some("\r"), 0),
                Key::Named(NamedKey::Escape) => (Some("\u{1b}"), 0),
                _ => (None, 0),
            };
            match crate::alert_keys::confirm_key(true, 0, chars, code) {
                crate::alert_keys::ConfirmKey::Accept => {
                    self.answer_close_banner(true);
                }
                crate::alert_keys::ConfirmKey::Cancel => {
                    self.answer_close_banner(false);
                }
                crate::alert_keys::ConfirmKey::PassThrough => {}
            }
            self.note_press_disposition(wid, &ev, None);
            return;
        }
        let overlay_repeat = self
            .palette_repeat_key(wid, mods, &ev)
            .filter(|_| !overlay_command_chord(mods, &ev));
        if self.on_key_overlay_mode(wid, mods, &ev) {
            self.note_press_disposition(
                wid,
                &ev,
                overlay_repeat.map(crate::LocalRepeatAction::Palette),
            );
            return;
        }
        if self.on_key_tab_menu_mode(wid, &ev) {
            self.note_press_disposition(wid, &ev, None);
            return;
        }
        let lens_copy = self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.stats_lens.open.is_some())
            && self.keybindings.action_for(
                base_key.get_or_insert_with(|| base_logical_key(&ev)),
                ev.physical_key,
                mods,
            ) == Some(keybinding::Action::Copy);
        if self.stats_lens_key(wid, mods, &ev.logical_key, lens_copy, inert) {
            self.note_press_disposition(wid, &ev, None);
            return;
        }
        #[cfg(any(windows, target_os = "linux"))]
        if let Some(ekey) = aterm_winit_keymap::map_logical_key(&ev.logical_key)
            && tab_menu_chord(
                self.config.tab_menu_chord_or_default(),
                keymap::modifiers_from_winit(mods),
                &ekey,
            )
            && !self.front_defers_tab_menu_chord(wid, &ekey)
            && self.open_active_tab_context_menu(wid)
        {
            self.note_press_disposition(wid, &ev, None);
            return;
        }
        let rename_repeat = self.rename_repeat_action(wid, mods, &ev);
        if self.on_key_rename_mode(wid, mods, &ev) {
            self.note_press_disposition(
                wid,
                &ev,
                rename_repeat
                    .map(|(session, edit)| crate::LocalRepeatAction::Rename { session, edit }),
            );
            return;
        }
        if self.on_key_native_mode(wid, mods, &ev) {
            return;
        }
        if self.on_key_claude_lights(wid, mods, &ev) {
            self.note_press_disposition(wid, &ev, None);
            return;
        }
        if !self.keybindings.is_empty() || !self.key_sequences.is_empty() {
            let base = base_key.get_or_insert_with(|| base_logical_key(&ev));
            match keybinding::resolve_chord(
                base,
                ev.physical_key,
                mods,
                &self.keybindings,
                &self.key_sequences,
                |dir| self.focus_pane_would_move(dir),
            ) {
                keybinding::ChordResolution::Action(action)
                    if find_suspends_action(action)
                        && self.windows.get(&wid).is_some_and(|ws| ws.search.is_some()) =>
                {
                    // Paste is the one suspended action the field honours:
                    // `field_edit_action` has no `v` arm and its printable tail
                    // refuses control chords, and off macOS the hardcoded ⌘V is
                    // Win+V (taken by the shell's clipboard history). Routing the
                    // resolved action here makes every spelling, including a
                    // user rebind, paste into the query (`search_paste_in` strips
                    // control bytes and stays single-line). Both paste actions
                    // paste the clipboard there, as a GTK entry's Shift+Insert
                    // does. Other suspended actions stay parked so the keystroke
                    // drives the field below.
                    if action.is_paste() {
                        self.search_paste_in(wid);
                        self.note_press_disposition(wid, &ev, None);
                        return;
                    }
                }
                keybinding::ChordResolution::Action(action) => {
                    let repeat_action = match action {
                        keybinding::Action::ScrollPageUp => Some(ScrollIntent::Up),
                        keybinding::Action::ScrollPageDown => Some(ScrollIntent::Down),
                        keybinding::Action::ScrollLineUp => Some(ScrollIntent::By(1)),
                        keybinding::Action::ScrollLineDown => Some(ScrollIntent::By(-1)),
                        keybinding::Action::ScrollToTop => Some(ScrollIntent::Top),
                        keybinding::Action::ScrollToBottom => Some(ScrollIntent::Bottom),
                        keybinding::Action::JumpPrevPrompt => Some(ScrollIntent::PrevPrompt),
                        keybinding::Action::JumpNextPrompt => Some(ScrollIntent::NextPrompt),
                        _ => None,
                    }
                    .and_then(|intent| {
                        self.front_terminal(wid)
                            .map(|terminal| crate::LocalRepeatAction::Scroll {
                                session: terminal.session,
                                intent,
                            })
                    });
                    let repeat_action = repeat_action.or(match action {
                        keybinding::Action::FontIncrease => {
                            Some(crate::LocalRepeatAction::FontZoom(
                                crate::FontZoomRepeatAction::Increase,
                            ))
                        }
                        keybinding::Action::FontDecrease => {
                            Some(crate::LocalRepeatAction::FontZoom(
                                crate::FontZoomRepeatAction::Decrease,
                            ))
                        }
                        keybinding::Action::FontReset => Some(crate::LocalRepeatAction::FontZoom(
                            crate::FontZoomRepeatAction::Reset,
                        )),
                        _ => None,
                    });
                    self.dispatch_action(wid, action);
                    self.note_press_disposition(wid, &ev, repeat_action);
                    return;
                }
                keybinding::ChordResolution::Sequence(bytes) => {
                    let search_active =
                        self.windows.get(&wid).is_some_and(|ws| ws.search.is_some());
                    if !search_active {
                        self.forward_literal_press(
                            wid,
                            ev.physical_key,
                            InputEvent::KeySequence(bytes),
                            None,
                        );
                        return;
                    }
                }
                keybinding::ChordResolution::FallThrough => {}
            }
        }
        if ((HARDCODED_SUPER_CHORDS && mods == ModifiersState::SUPER)
            || mods == ModifiersState::CONTROL)
            && let Some(forward) = terminal_emacs_search_direction(
                base_key.get_or_insert_with(|| base_logical_key(&ev)),
                mods,
                || self.front_agent_composer(wid),
            )
        {
            self.terminal_emacs_search_pressed(wid, ev.physical_key, forward);
            return;
        }
        if HARDCODED_SUPER_CHORDS && self.on_key_super_shift_chord(mods, &ev) {
            self.note_consumed_press(wid, &ev);
            return;
        }
        if HARDCODED_SUPER_CHORDS && self.on_key_super_chord(mods, &ev) {
            self.note_consumed_press(wid, &ev);
            return;
        }
        if HARDCODED_SUPER_CHORDS && self.on_key_pane_focus(mods, &ev) {
            self.note_consumed_press(wid, &ev);
            return;
        }
        if let Some(intent) = scrollback_chord(mods, &ev)
            .filter(|_| self.windows.get(&wid).is_none_or(|ws| ws.search.is_none()))
            .filter(|_| !self.front_alternate_screen(wid))
        {
            if let Some(session) = self.front_terminal(wid).map(|terminal| terminal.session) {
                self.note_local_repeat_press(
                    wid,
                    ev.physical_key,
                    crate::LocalRepeatAction::Scroll { session, intent },
                );
            } else {
                self.note_consumed_press(wid, &ev);
            }
            self.input(wid, InputEvent::ScrollView(intent), Source::Human);
            return;
        }
        if HARDCODED_SUPER_CHORDS
            && mods.super_key()
            && let Key::Character(s) = &ev.logical_key
            && s.eq_ignore_ascii_case("f")
        {
            self.find_requested_in(wid);
            self.note_consumed_press(wid, &ev);
            return;
        }
        let search_repeat = self
            .search_repeat_action(wid, mods, &ev)
            .and_then(|action| self.front_terminal(wid).map(|t| (t.session, action)));
        if self.on_key_search_mode(wid, mods, &ev) {
            self.note_press_disposition(
                wid,
                &ev,
                search_repeat
                    .map(|(session, action)| crate::LocalRepeatAction::Search { session, action }),
            );
            return;
        }
        if HARDCODED_SUPER_CHORDS
            && mods.super_key()
            && let Key::Character(s) = &ev.logical_key
            && s.eq_ignore_ascii_case("c")
        {
            self.copy_selection_in(wid);
            self.note_consumed_press(wid, &ev);
            return;
        }
        if !inert {
            self.cancel_press_scroll_motion(wid);
        }
        if hardcoded_paste_chord(mods, &ev) {
            self.snap_to_bottom(wid);
            self.paste_clipboard();
            self.note_consumed_press(wid, &ev);
            return;
        }
        let zoom_repeat = font_zoom_repeat_action(mods, &ev);
        if self.on_key_font_zoom(mods, &ev) {
            self.note_press_disposition(
                wid,
                &ev,
                zoom_repeat.map(crate::LocalRepeatAction::FontZoom),
            );
            return;
        }
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| keymap::suppress_direct_send(&ws.preedit))
        {
            self.snap_to_bottom(wid);
            self.note_consumed_press(wid, &ev);
            return;
        }
        if !self.option_as_meta
            && mods.alt_key()
            && !mods.control_key()
            && !mods.super_key()
            && let Some(text) = &ev.text
            && !text.is_empty()
            && !text.chars().any(char::is_control)
        {
            self.forward_literal_press(
                wid,
                ev.physical_key,
                InputEvent::Text(text.to_string()),
                None,
            );
            return;
        }
        if HARDCODED_SUPER_CHORDS && mods.super_key() {
            self.note_consumed_press(wid, &ev);
            return;
        }
        if inert && !self.front_kitty_reports_all_keys(wid) {
            self.note_consumed_press(wid, &ev);
            return;
        }
        if let Some(InputEvent::Key {
            key,
            mods: mapped_mods,
            base_layout,
            layout_text,
            ..
        }) = press
        {
            let km_mods = mapped_mods | (self.lock_modifiers)();
            self.note_forwarded_press_key(
                wid,
                ev.physical_key,
                false,
                key.clone(),
                km_mods,
                base_layout,
                layout_text,
            );
            self.input(
                wid,
                InputEvent::Key {
                    key,
                    mods: km_mods,
                    base_layout,
                    layout_text,
                    event_type: aterm_types::keyboard::KeyEventType::Press,
                },
                Source::Human,
            );
            return;
        }
        if let Some(text @ InputEvent::Text(_)) = press {
            let repeat = keymap::text_press_repeat(&ev, keymap::modifiers_from_winit(mods))
                .map(InputEvent::Text);
            self.forward_literal_press(wid, ev.physical_key, text, repeat);
            return;
        }
        self.note_consumed_press(wid, &ev);
    }

    /// Whether an agent's composer owns the front terminal's prompt.
    fn front_agent_composer(&self, wid: WindowId) -> bool {
        let Some(pooled) = self
            .front_terminal(wid)
            .and_then(|terminal| self.pool.get(terminal.session))
        else {
            return false;
        };
        let (agent, group) = {
            let timeline = pooled
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let agent = timeline.agent();
            (
                crate::harness_host::identified_agent(agent).is_some(),
                agent.program_pgid,
            )
        };
        if !agent {
            return false;
        }
        let front = if pooled.master >= 0 {
            (self.session_factory.fg_probe)(pooled.master)
        } else {
            -1
        };
        (front <= 0 || front == group) && !(self.session_factory.handed_terminal)(group)
    }

    /// Whether the front terminal shows its alternate screen, which keeps no scrollback.
    fn front_alternate_screen(&self, wid: WindowId) -> bool {
        self.front_terminal(wid)
            .and_then(|t| self.pool.get(t.session))
            .is_some_and(|s| s.ctx.modes.alternate_screen())
    }

    /// Whether the front terminal's keyboard mode carries kitty `REPORT_ALL_KEYS_AS_ESC`.
    fn front_kitty_reports_all_keys(&self, wid: WindowId) -> bool {
        self.front_terminal(wid)
            .and_then(|t| self.pool.get(t.session))
            .is_some_and(|s| {
                s.ctx
                    .modes
                    .keyboard_mode()
                    .contains(aterm_types::keyboard::KeyboardMode::REPORT_ALL_KEYS_AS_ESC)
            })
    }

    /// Hardcoded Cmd-Shift chords of [`Self::on_key`].
    fn on_key_super_shift_chord(&mut self, mods: ModifiersState, ev: &KeyEvent) -> bool {
        if mods.super_key()
            && mods.shift_key()
            && let Key::Character(s) = &ev.logical_key
        {
            match s.as_str() {
                "]" | "}" => {
                    self.cycle_tab(true);
                    return true;
                }
                "[" | "{" => {
                    self.cycle_tab(false);
                    return true;
                }
                "t" | "T" => {
                    let _ = self.reopen_last_closed_tab();
                    return true;
                }
                "n" | "N" => {
                    if let Some(proxy) = self.proxy.as_ref() {
                        let _ = proxy.send_event(Wake::DetachActiveTab);
                    }
                    return true;
                }
                "d" | "D" => {
                    self.split_focused_pane(pane::SplitDir::Horizontal);
                    return true;
                }
                "o" | "O" => {
                    if let Some(proxy) = self.proxy.as_ref() {
                        let _ = proxy.send_event(Wake::ViewActiveSessionInNewWindow);
                    }
                    return true;
                }
                "m" | "M" => {
                    let _ = self.migrate_active_tab_to_next_window();
                    return true;
                }
                "l" | "L" => {
                    // The chord has no `wid`: it acts on the front window, as
                    // every chord here does.
                    if let Some(wid) = self.frontmost_window {
                        self.open_session_ledger(wid);
                    }
                    return true;
                }
                _ => {}
            }
        }
        false
    }

    /// Hardcoded Cmd (no Shift) chords of [`Self::on_key`].
    fn on_key_super_chord(&mut self, mods: ModifiersState, ev: &KeyEvent) -> bool {
        if mods.super_key()
            && !mods.shift_key()
            && let Key::Character(s) = &ev.logical_key
        {
            let lc = s.to_ascii_lowercase();
            match lc.as_str() {
                "n" => {
                    self.open_new_window();
                    return true;
                }
                "t" => {
                    self.open_tab();
                    return true;
                }
                "d" => {
                    self.split_focused_pane(pane::SplitDir::Vertical);
                    return true;
                }
                "q" => {
                    self.post_menu_action(crate::menu::MenuAction::Quit);
                    return true;
                }
                "w" => {
                    // The last pane of the last tab sets `pending_close` on the window
                    // `close_active_tab` closed (the frontmost, not the event's `wid`,
                    // which can differ); `window_event`, which has the
                    // `ActiveEventLoop`, closes it after `on_key` returns.
                    if let Some(closed) = self.close_active_tab()
                        && let Some(ws) = self.windows.get_mut(&closed)
                    {
                        ws.pending_close = true;
                    }
                    return true;
                }
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" => {
                    if let Some(d) = lc.chars().next().and_then(|c| c.to_digit(10)) {
                        self.switch_tab(d as usize - 1);
                    }
                    return true;
                }
                _ => {}
            }
        }
        false
    }

    /// Pane chords (iTerm2 parity).
    fn on_key_pane_focus(&mut self, mods: ModifiersState, ev: &KeyEvent) -> bool {
        // Cmd-Shift-Enter (no Alt): toggle pane zoom.
        if mods.super_key()
            && mods.shift_key()
            && !mods.alt_key()
            && matches!(&ev.logical_key, Key::Named(NamedKey::Enter))
        {
            self.toggle_pane_zoom();
            return true;
        }
        if !(mods.super_key() && mods.alt_key()) {
            return false;
        }
        let dir = match &ev.logical_key {
            Key::Named(NamedKey::ArrowLeft) => pane::FocusDir::Left,
            Key::Named(NamedKey::ArrowRight) => pane::FocusDir::Right,
            Key::Named(NamedKey::ArrowUp) => pane::FocusDir::Up,
            Key::Named(NamedKey::ArrowDown) => pane::FocusDir::Down,
            _ => return false,
        };
        self.focus_pane(dir);
        true
    }

    /// Classify a find-mode keystroke into the repeatable action it drives.
    fn search_repeat_action(
        &self,
        wid: WindowId,
        mods: ModifiersState,
        ev: &KeyEvent,
    ) -> Option<crate::SearchRepeatAction> {
        use crate::SearchRepeatAction as Action;

        if self.windows.get(&wid).is_none_or(|ws| ws.search.is_none()) {
            return None;
        }
        let base = field_chord_key(ev, mods);
        let base_is =
            |want: &str| matches!(&base, Key::Character(c) if c.eq_ignore_ascii_case(want));
        let ctrl = mods.control_key() && !mods.super_key() && !mods.alt_key();
        let bare_cmd =
            mods.super_key() && !mods.shift_key() && !mods.control_key() && !mods.alt_key();
        match &ev.logical_key {
            Key::Named(NamedKey::ArrowDown) => return Some(Action::Step(true)),
            Key::Named(NamedKey::ArrowUp) => return Some(Action::Step(false)),
            #[cfg(not(target_os = "macos"))]
            Key::Named(NamedKey::F3)
                if !mods.control_key() && !mods.alt_key() && !mods.super_key() =>
            {
                return Some(Action::Repeat(!mods.shift_key()));
            }
            _ if (ctrl || bare_cmd) && base_is("s") => return Some(Action::Repeat(true)),
            _ if (ctrl || bare_cmd) && base_is("r") => return Some(Action::Repeat(false)),
            _ => {}
        }
        field_edit_action(mods, ev).map(Action::Edit)
    }

    /// Classify a keystroke aimed at the in-grid rename field into its repeatable edit.
    fn rename_repeat_action(
        &self,
        wid: WindowId,
        mods: ModifiersState,
        ev: &KeyEvent,
    ) -> Option<(u64, crate::app_search::SearchEdit)> {
        let session = self.inline_rename_edit(wid)?.session;
        Some((session, field_edit_action(mods, ev)?))
    }

    /// Open a directed terminal search or repeat an already-open one.
    pub(crate) fn terminal_emacs_search_pressed(
        &mut self,
        wid: WindowId,
        physical_key: aterm_window::keyboard::PhysicalKey,
        forward: bool,
    ) {
        let session = self.front_terminal(wid).map(|terminal| terminal.session);
        if self
            .windows
            .get(&wid)
            .is_some_and(|window| window.search.is_some())
        {
            self.search_repeat_in(wid, forward);
        } else {
            self.search_enter_direction_in(wid, forward);
        }
        if let Some(session) = session {
            self.note_local_repeat_press(
                wid,
                physical_key,
                crate::LocalRepeatAction::Search {
                    session,
                    action: crate::SearchRepeatAction::Repeat(forward),
                },
            );
        } else {
            self.note_consumed_press_key(wid, physical_key, false);
        }
    }

    fn apply_search_repeat_action(&mut self, wid: WindowId, action: crate::SearchRepeatAction) {
        match action {
            crate::SearchRepeatAction::Step(forward) => self.search_step_in(wid, forward),
            crate::SearchRepeatAction::Repeat(forward) => self.search_repeat_in(wid, forward),
            crate::SearchRepeatAction::Edit(edit) => self.search_edit_in(wid, edit),
        }
    }

    /// A live IME composition owns its keystrokes.
    fn grid_ime_composing(&self, wid: WindowId) -> bool {
        self.windows
            .get(&wid)
            .is_some_and(|ws| !ws.preedit.is_empty())
    }

    /// In-grid rename dispatch of [`Self::on_key`].
    fn on_key_rename_mode(&mut self, wid: WindowId, mods: ModifiersState, ev: &KeyEvent) -> bool {
        let Some(session) = self.inline_rename_edit(wid).map(|edit| edit.session) else {
            return false;
        };
        if self.grid_ime_composing(wid) {
            return true;
        }
        if is_bare_modifier_key(&ev.logical_key) {
            return true;
        }
        if let Some((_, edit)) = self.rename_repeat_action(wid, mods, ev) {
            self.rename_field_edit(wid, edit);
            return true;
        }
        let base = base_logical_key(ev);
        let bare_cmd =
            mods.super_key() && !mods.shift_key() && !mods.control_key() && !mods.alt_key();
        let paste_chord = self
            .keybindings
            .action_for(&base, ev.physical_key, mods)
            .is_some_and(keybinding::Action::is_paste);
        match &ev.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.cancel_session_rename(wid, session);
                return true;
            }
            Key::Named(NamedKey::Enter | NamedKey::Tab) => {
                let text = self
                    .inline_rename_edit(wid)
                    .map(|edit| edit.text.clone())
                    .unwrap_or_default();
                self.commit_session_rename(wid, session, &text);
                return true;
            }
            _ if bare_cmd && matches!(&base, Key::Character(c) if c.eq_ignore_ascii_case("v")) => {
                self.rename_paste_in(wid);
                return true;
            }
            _ if paste_chord => {
                self.rename_paste_in(wid);
                return true;
            }
            _ => {}
        }
        self.settle_rename_edit(wid);
        false
    }

    /// The engine-neutral twin of [`Self::on_key_rename_mode`] for controller `key`/`text`/`paste`
    /// verbs.
    pub(crate) fn rename_input_event(
        &mut self,
        wid: WindowId,
        ev: &crate::input::InputEvent,
    ) -> bool {
        use crate::app_search::SearchEdit;
        use crate::input::InputEvent;
        use aterm_types::keyboard::{Key as TKey, KeyEventType, NamedKey as TNamed};
        let Some(session) = self.inline_rename_edit(wid).map(|edit| edit.session) else {
            return false;
        };
        let folded = ev.keypad_folded();
        let ev = folded.as_ref().unwrap_or(ev);
        match ev {
            InputEvent::Key {
                key, event_type, ..
            } => {
                // A release is not an edit; the caller's consumed-press set
                // swallows the pair before this gate.
                if matches!(event_type, KeyEventType::Release) {
                    return false;
                }
                match key {
                    TKey::Named(TNamed::Escape) => self.cancel_session_rename(wid, session),
                    TKey::Named(TNamed::Enter | TNamed::Tab) => {
                        let text = self
                            .inline_rename_edit(wid)
                            .map(|edit| edit.text.clone())
                            .unwrap_or_default();
                        self.commit_session_rename(wid, session, &text);
                    }
                    TKey::Named(TNamed::Backspace) => {
                        self.rename_field_edit(wid, SearchEdit::DeleteBack);
                    }
                    TKey::Named(TNamed::Delete) => {
                        self.rename_field_edit(wid, SearchEdit::DeleteForward);
                    }
                    TKey::Named(TNamed::ArrowLeft) => {
                        self.rename_field_edit(wid, SearchEdit::MoveCharLeft);
                    }
                    TKey::Named(TNamed::ArrowRight) => {
                        self.rename_field_edit(wid, SearchEdit::MoveCharRight);
                    }
                    TKey::Named(TNamed::Home) => {
                        self.rename_field_edit(wid, SearchEdit::MoveStart);
                    }
                    TKey::Named(TNamed::End) => {
                        self.rename_field_edit(wid, SearchEdit::MoveEnd);
                    }
                    TKey::Named(TNamed::Space) => {
                        self.rename_field_edit(wid, SearchEdit::Insert(" ".to_string()));
                    }
                    TKey::Character(c) if !c.is_control() => {
                        self.rename_field_edit(wid, SearchEdit::Insert(c.to_string()));
                    }
                    _ => {}
                }
                true
            }
            InputEvent::Text(text) | InputEvent::Paste(text, _) => {
                self.rename_field_edit(wid, SearchEdit::Insert(text.clone()));
                true
            }
            _ => false,
        }
    }

    /// Find-mode keystroke dispatch of [`Self::on_key`].
    fn on_key_search_mode(&mut self, wid: WindowId, mods: ModifiersState, ev: &KeyEvent) -> bool {
        if self.windows.get(&wid).is_none_or(|ws| ws.search.is_none()) {
            return false;
        }
        if self.grid_ime_composing(wid) {
            return true;
        }
        let base = field_chord_key(ev, mods);
        let base_is =
            |want: &str| matches!(&base, Key::Character(c) if c.eq_ignore_ascii_case(want));
        let ctrl = mods.control_key() && !mods.super_key() && !mods.alt_key();
        let cmd_alt = mods.super_key() && mods.alt_key() && !mods.control_key();
        let bare_cmd =
            mods.super_key() && !mods.shift_key() && !mods.control_key() && !mods.alt_key();
        if bare_cmd && base_is("c") {
            return false;
        }
        #[cfg(not(target_os = "macos"))]
        {
            let bare_alt = mods.alt_key() && !mods.control_key() && !mods.super_key();
            if bare_alt && base_is("c") {
                self.search_toggle_case_in(wid);
                return true;
            }
            if bare_alt && base_is("r") {
                self.search_toggle_regex_in(wid);
                return true;
            }
        }
        if let Some(action) = self.search_repeat_action(wid, mods, ev) {
            self.apply_search_repeat_action(wid, action);
            return true;
        }
        match &ev.logical_key {
            Key::Named(NamedKey::Escape) => self.search_cancel_in(wid),
            Key::Named(NamedKey::Enter) => self.search_accept_in(wid),
            _ if ctrl && base_is("g") => self.search_cancel_in(wid),
            _ if bare_cmd && base_is("v") => self.search_paste_in(wid),
            _ if cmd_alt && base_is("c") => self.search_toggle_case_in(wid),
            _ if cmd_alt && base_is("r") => self.search_toggle_regex_in(wid),
            _ => {}
        }
        true
    }

    /// The window half of the press-path viewport snap.
    fn cancel_press_scroll_motion(&mut self, wid: WindowId) {
        self.drop_front_scroll_motion(wid);
    }

    /// Drop the window's wheel glide.
    pub(crate) fn drop_front_scroll_motion(&mut self, wid: WindowId) {
        let front = self
            .front_terminal(wid)
            .map(|terminal| terminal.term.clone());
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let elsewhere = ws.scroll_glide.as_ref().is_some_and(|glide| {
            front
                .as_ref()
                .is_none_or(|front| !Arc::ptr_eq(&glide.term, front))
        });
        if elsewhere {
            return;
        }
        ws.scroll_glide = None;
        ws.overscroll = None;
        ws.scroll_frac_px = 0;
    }

    /// What a held key repeats against the open palette.
    fn palette_repeat_key(
        &self,
        wid: WindowId,
        mods: ModifiersState,
        ev: &KeyEvent,
    ) -> Option<crate::app_palette::PaletteKey> {
        use crate::app_palette::PaletteKey;

        if self
            .windows
            .get(&wid)
            .and_then(|ws| ws.overlay.as_ref())
            .map(|overlay| overlay.kind())
            != Some(crate::overlay::OverlayKind::Palette)
        {
            return None;
        }
        match crate::app_palette::palette_key(mods, ev) {
            key @ (PaletteKey::Move(_) | PaletteKey::Edit(_)) => Some(key),
            PaletteKey::Close | PaletteKey::Activate | PaletteKey::Inert => None,
        }
    }

    /// The modal-overlay key gate.
    fn on_key_overlay_mode(&mut self, wid: WindowId, mods: ModifiersState, ev: &KeyEvent) -> bool {
        use crate::overlay::OverlayKind;
        let kind = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.overlay.as_ref())
            .map(|o| o.kind());
        if kind.is_some() && overlay_command_chord(mods, ev) {
            return true;
        }
        match kind {
            Some(OverlayKind::Palette) => self.on_key_palette_mode(wid, mods, ev),
            Some(OverlayKind::Overview) => self.on_key_overview_mode(wid, ev),
            None => false,
        }
    }

    /// While a tab context menu is open on `wid`, drive it from the keyboard.
    fn on_key_tab_menu_mode(&mut self, wid: WindowId, ev: &KeyEvent) -> bool {
        if !self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.tab_menu.is_some())
        {
            return false;
        }
        let nav = if crate::keymap::press_is_inert(ev) {
            crate::tab_menu::MenuNav::Inert
        } else {
            aterm_winit_keymap::map_logical_key(&ev.logical_key)
                .as_ref()
                .map_or(
                    crate::tab_menu::MenuNav::Dismiss,
                    crate::tab_menu::nav_for_key,
                )
        };
        self.tab_menu_nav(wid, nav)
    }

    /// Apply one [`MenuNav`](crate::tab_menu::MenuNav) to `wid`'s open card.
    fn tab_menu_nav(&mut self, wid: WindowId, nav: crate::tab_menu::MenuNav) -> bool {
        use crate::tab_menu::MenuNav;
        let Some((entries, highlight, limit)) = self.windows.get(&wid).and_then(|ws| {
            let menu = ws.tab_menu.as_ref()?;
            let limit = ws
                .tab_menu_rect
                .map_or(menu.entries.len(), crate::tab_menu::visible_entries);
            Some((menu.entries.clone(), menu.highlight, limit))
        }) else {
            return false;
        };
        let step = |from, down| crate::tab_menu::step_selectable(&entries, from, down, limit);
        match nav {
            MenuNav::Activate => match highlight {
                Some(i) => self.activate_tab_menu_entry(wid, i),
                None => {
                    self.close_tab_menu(wid);
                }
            },
            MenuNav::Dismiss => {
                self.close_tab_menu(wid);
            }
            MenuNav::Down => self.set_tab_menu_highlight(wid, step(highlight, true)),
            MenuNav::Up => self.set_tab_menu_highlight(wid, step(highlight, false)),
            MenuNav::First => self.set_tab_menu_highlight(wid, step(None, true)),
            MenuNav::Last => self.set_tab_menu_highlight(wid, step(None, false)),
            MenuNav::Inert => {}
        }
        true
    }

    /// Whether the front terminal's application negotiated the keyboard enhancement that makes
    /// `key` reportable.
    #[cfg(not(target_os = "macos"))]
    pub(crate) fn front_defers_tab_menu_chord(
        &self,
        wid: WindowId,
        key: &aterm_types::keyboard::Key,
    ) -> bool {
        use aterm_types::keyboard::{Key as EKey, NamedKey as ENamed};
        let Some(term) = self.front_terminal(wid).map(|m| m.term.clone()) else {
            return false;
        };
        let tl = crate::term_lock(&term);
        match key {
            EKey::Named(ENamed::ContextMenu) => tl.kitty_reports_functional_keys(),
            EKey::Named(ENamed::F10) => tl.kitty_report_all_keys(),
            _ => false,
        }
    }

    /// The convergence-seam twin of [`Self::on_key_tab_menu_mode`] plus the `on_key` chord arm.
    pub(crate) fn tab_menu_input_event(
        &mut self,
        wid: WindowId,
        ev: &crate::input::InputEvent,
    ) -> bool {
        use crate::input::InputEvent;
        use aterm_types::keyboard::KeyEventType;
        let open = self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.tab_menu.is_some());
        if open {
            return match ev {
                InputEvent::Key {
                    event_type: KeyEventType::Release,
                    ..
                } => {
                    // An untracked release belongs to a press that predates the card
                    // and must still reach a Kitty `REPORT_EVENT_TYPES` app.
                    false
                }
                InputEvent::Key { key, .. } => {
                    self.tab_menu_nav(wid, crate::tab_menu::nav_for_key(key))
                }
                InputEvent::Text(_) | InputEvent::KeySequence(_) | InputEvent::Paste(..) => {
                    self.close_tab_menu(wid);
                    true
                }
                InputEvent::MouseButton {
                    button,
                    pressed: true,
                    ..
                } => {
                    self.close_tab_menu(wid);
                    if let Some(ws) = self.windows.get_mut(&wid) {
                        ws.tab_menu_consumed_button = Some(*button);
                    }
                    true
                }
                InputEvent::MouseButton { .. }
                | InputEvent::MouseMove { .. }
                | InputEvent::Wheel { .. }
                | InputEvent::ScrollView(_) => true,
                _ => false,
            };
        }
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.tab_menu_consumed_button.is_some())
        {
            match ev {
                InputEvent::MouseButton {
                    button,
                    pressed: false,
                    ..
                } => {
                    if let Some(ws) = self.windows.get_mut(&wid)
                        && ws.tab_menu_consumed_button == Some(*button)
                    {
                        ws.tab_menu_consumed_button = None;
                        return true;
                    }
                }
                InputEvent::MouseMove { .. } => return true,
                _ => {}
            }
        }

        #[cfg(any(windows, target_os = "linux"))]
        if let InputEvent::Key {
            key,
            mods,
            event_type: KeyEventType::Press,
            ..
        } = ev
            && tab_menu_chord(self.config.tab_menu_chord_or_default(), *mods, key)
            && !self.front_defers_tab_menu_chord(wid, key)
        {
            if self.controller_card_refused() {
                return true;
            }
            return self.open_active_tab_context_menu(wid);
        }
        false
    }

    /// Set the open menu's highlight and repaint if it moved.
    fn set_tab_menu_highlight(&mut self, wid: WindowId, to: Option<usize>) {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let Some(menu) = ws.tab_menu.as_mut() else {
            return;
        };
        if menu.highlight == to {
            return;
        }
        menu.highlight = to;
        if let Some(w) = &ws.os_window {
            w.request_redraw();
        }
    }

    /// Cmd-= / Cmd-+ / Cmd-- / Cmd-0 live font zoom (grow / shrink / reset) of [`Self::on_key`].
    fn on_key_font_zoom(&mut self, mods: ModifiersState, ev: &KeyEvent) -> bool {
        match font_zoom_repeat_action(mods, ev) {
            Some(crate::FontZoomRepeatAction::Increase) => {
                self.set_font_size(self.font_size + FONT_ZOOM_STEP);
            }
            Some(crate::FontZoomRepeatAction::Decrease) => {
                self.set_font_size(self.font_size - FONT_ZOOM_STEP);
            }
            Some(crate::FontZoomRepeatAction::Reset) => {
                self.set_font_size(self.default_font_size);
            }
            None => return false,
        }
        true
    }

    /// Run a user-bound [`keybinding::Action`].
    pub(crate) fn dispatch_action(&mut self, wid: WindowId, action: keybinding::Action) {
        use keybinding::Action;
        self.note_update_handoff_activity();
        match action {
            Action::NewTab => self.open_tab(),
            Action::ReopenClosedTab => {
                let _ = self.reopen_last_closed_tab();
            }
            Action::CloseTab => {
                let _ = wid;
                if let Some(closed) = self.close_active_tab()
                    && let Some(ws) = self.windows.get_mut(&closed)
                {
                    ws.pending_close = true;
                }
            }
            Action::NewWindow => self.open_new_window(),
            Action::NextTab => self.cycle_tab(true),
            Action::PrevTab => self.cycle_tab(false),
            Action::SwitchTab(n) => self.switch_tab(usize::from(n).saturating_sub(1)),
            Action::SplitVertical => self.split_focused_pane(pane::SplitDir::Vertical),
            Action::SplitHorizontal => self.split_focused_pane(pane::SplitDir::Horizontal),
            Action::FocusPaneLeft => self.focus_pane(pane::FocusDir::Left),
            Action::FocusPaneRight => self.focus_pane(pane::FocusDir::Right),
            Action::FocusPaneUp => self.focus_pane(pane::FocusDir::Up),
            Action::FocusPaneDown => self.focus_pane(pane::FocusDir::Down),
            Action::TogglePaneZoom => self.toggle_pane_zoom(),
            Action::Copy => {
                self.copy_selection_in(wid);
            }
            Action::Paste => {
                self.snap_to_bottom(wid);
                self.paste_clipboard();
            }
            Action::PasteSelection => {
                self.snap_to_bottom(wid);
                #[cfg(target_os = "linux")]
                self.paste_primary_into(wid);
                #[cfg(not(target_os = "linux"))]
                self.paste_clipboard();
            }
            Action::Find => self.find_requested_in(wid),
            Action::FontIncrease => self.set_font_size(self.font_size + FONT_ZOOM_STEP),
            Action::FontDecrease => self.set_font_size(self.font_size - FONT_ZOOM_STEP),
            Action::FontReset => self.set_font_size(self.default_font_size),
            Action::ScrollPageUp => {
                self.input(wid, InputEvent::ScrollView(ScrollIntent::Up), Source::Human);
            }
            Action::ScrollPageDown => {
                self.input(
                    wid,
                    InputEvent::ScrollView(ScrollIntent::Down),
                    Source::Human,
                );
            }
            Action::ScrollLineUp => {
                self.input(
                    wid,
                    InputEvent::ScrollView(ScrollIntent::By(1)),
                    Source::Human,
                );
            }
            Action::ScrollLineDown => {
                self.input(
                    wid,
                    InputEvent::ScrollView(ScrollIntent::By(-1)),
                    Source::Human,
                );
            }
            Action::ScrollToTop => {
                self.input(
                    wid,
                    InputEvent::ScrollView(ScrollIntent::Top),
                    Source::Human,
                );
            }
            Action::ScrollToBottom => {
                self.input(
                    wid,
                    InputEvent::ScrollView(ScrollIntent::Bottom),
                    Source::Human,
                );
            }
            Action::JumpPrevPrompt => {
                self.input(
                    wid,
                    InputEvent::ScrollView(ScrollIntent::PrevPrompt),
                    Source::Human,
                );
            }
            Action::JumpNextPrompt => {
                self.input(
                    wid,
                    InputEvent::ScrollView(ScrollIntent::NextPrompt),
                    Source::Human,
                );
            }
            Action::ToggleSettings => {
                if self.headless {
                    let _ = self.open_settings_tab(crate::native_settings::SettingsRoute::Home);
                } else {
                    self.post_menu_action(crate::menu::MenuAction::ToggleSettings);
                }
            }
            Action::ToggleAbout => {
                let _ = self.open_settings_tab(crate::native_settings::SettingsRoute::About);
            }
            Action::ToggleMatrixRain => self.toggle_matrix_rain(wid),
            Action::ToggleSeriousMode => {
                self.user_toggle_serious_mode();
            }
            Action::OpenPalette => self.toggle_palette(),
            Action::ChooseTheme => self.open_theme_palette(),
            Action::RenameSession => {
                self.begin_active_session_rename(wid);
            }
            Action::FindNext => {
                let _ = self.search_find_again_in(wid, true);
            }
            Action::FindPrev => {
                let _ = self.search_find_again_in(wid, false);
            }
            Action::OpenLedger => self.open_session_ledger(wid),
            Action::SelectAll => {
                if self.find_field_window().is_none() {
                    self.select_all();
                }
            }
            Action::ToggleFullscreen => {
                let _ = self.toggle_fullscreen();
            }
        }
    }

    /// A composition update (`Ime::Preedit`).
    pub(crate) fn on_ime_preedit(
        &mut self,
        wid: WindowId,
        text: String,
        cursor: Option<(usize, usize)>,
    ) {
        self.clear_move_license(wid);
        let mut changed = false;
        if let Some(ws) = self.windows.get_mut(&wid) {
            let caret = cursor.map(|(start, _end)| start);
            changed = ws.preedit != text || ws.preedit_caret != caret;
            ws.preedit.clone_from(&text);
            ws.preedit_caret = caret;
            ws.ime_compose =
                ime_compose_on_preedit(ws.ime_compose, !text.is_empty(), std::time::Instant::now());
        }
        if self.active_native_view(wid).is_some() {
            let _ = self.dispatch_native_event(
                wid,
                crate::native_app::AppEvent::TextInput(
                    crate::native_app::TextInputEvent::Preedit {
                        text,
                        selection: cursor.map(|(start, end)| start..end),
                    },
                ),
            );
            if let Some(window) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                window.request_redraw();
            }
            return;
        }
        if changed
            && !text.is_empty()
            && let Some(session) = self.focused_session_id(wid)
            && let Some(owner) = self.pool.get(session)
        {
            let _ = note_person_reaching(&owner.ctx, crate::human_input::Gesture::Other, false);
        }
        if changed && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
    }

    /// The platform turned text input on or off (`Ime::Enabled` / `Ime::Disabled`).
    pub(crate) fn on_ime_enablement_changed(&mut self, wid: WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.last_ime_rect = None;
        }
        self.on_ime_preedit(wid, String::new(), None);
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
    }

    /// Tell the platform where the caret is.
    pub(crate) fn report_ime_cursor_area(
        &mut self,
        wid: WindowId,
        cur: (u16, u16),
        off: (u16, u16),
        vis: bool,
    ) {
        if !vis {
            return;
        }
        let frame_cell = (
            (cur.0.saturating_add(off.0)) as usize + usize::from(self.chrome_rows(wid)),
            (cur.1.saturating_add(off.1)) as usize,
        );
        self.report_ime_cursor_area_frame(wid, frame_cell);
    }

    /// [`Self::report_ime_cursor_area`]'s core.
    pub(crate) fn report_ime_cursor_area_frame(
        &mut self,
        wid: WindowId,
        frame_cell: (usize, usize),
    ) {
        let (cw, ch) = self.win_cell_size(wid);
        let pad = self.win_pad(wid);
        let pad_top = self.win_pad_top(wid);
        let head = self.win_head(wid);
        let (band_x, band_y) = self.frame_origin(wid);
        let x = i64::try_from(frame_cell.1 * cw.max(1) + pad)
            .unwrap_or(i64::MAX)
            .saturating_add(band_x);
        let y = i64::try_from(frame_cell.0 * ch.max(1) + pad_top + head)
            .unwrap_or(i64::MAX)
            .saturating_add(band_y);
        let rect = (
            i32::try_from(x).unwrap_or(i32::MAX),
            i32::try_from(y).unwrap_or(i32::MAX),
            u32::try_from(cw.max(1)).unwrap_or(u32::MAX),
            u32::try_from(ch.max(1)).unwrap_or(u32::MAX),
        );
        self.report_ime_cursor_rect(wid, rect);
    }

    /// Window-space physical caret box shared by terminal cells and native text fields.
    pub(crate) fn report_ime_cursor_rect(&mut self, wid: WindowId, rect: (i32, i32, u32, u32)) {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if ws.last_ime_rect == Some(rect) {
            return;
        }
        ws.last_ime_rect = Some(rect);
        if let Some(window) = ws.os_window.as_ref() {
            window.set_ime_cursor_area(
                aterm_window::dpi::PhysicalPosition::new(rect.0, rect.1),
                aterm_window::dpi::PhysicalSize::new(rect.2, rect.3),
            );
        }
    }

    /// Composition committed (`Ime::Commit`).
    pub(crate) fn on_ime_commit(&mut self, wid: WindowId, text: String) {
        let route = self.ime_route(wid);
        let as_paste = keymap::committed_text_is_paste(&text) && self.typing_reaches_grid(wid);
        let reaches_pty = !text.is_empty() && matches!(route, PreeditOwner::Grid);
        if as_paste {
            self.clear_move_license_keeping_hand(wid);
        } else if reaches_pty {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.fx.cursor_mut().glow.supersede_typed_press();
            }
        } else {
            self.clear_move_license(wid);
        }
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.preedit.clear();
            ws.preedit_caret = None;
            if let Some((started, _)) = ws.ime_compose.take() {
                if text.is_empty() {
                    crate::metrics::note_ime_cancelled();
                } else {
                    let ns = std::time::Instant::now()
                        .saturating_duration_since(started)
                        .as_nanos();
                    crate::metrics::note_ime_compose(u64::try_from(ns).unwrap_or(u64::MAX));
                }
            }
        }
        match route {
            PreeditOwner::Native => {
                if !text.is_empty() {
                    let _ = self.dispatch_native_event(
                        wid,
                        crate::native_app::AppEvent::TextInput(
                            crate::native_app::TextInputEvent::Commit(text),
                        ),
                    );
                }
                if let Some(window) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                    window.request_redraw();
                }
            }
            _ if text.is_empty() => {}
            PreeditOwner::Rename => {
                self.rename_field_edit(wid, crate::app_search::SearchEdit::Insert(text));
            }
            PreeditOwner::Find => {
                self.search_edit_in(wid, crate::app_search::SearchEdit::Insert(text));
                if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                    w.request_redraw();
                }
            }
            PreeditOwner::Grid if as_paste => {
                self.snap_to_bottom(wid);
                let session = self.focused_session_id(wid);
                self.deliver_paste(wid, session, text, Source::Human);
                if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                    w.request_redraw();
                }
            }
            PreeditOwner::Grid | PreeditOwner::None => {
                self.input(wid, InputEvent::Text(text), Source::Human);
                if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                    w.request_redraw();
                }
            }
        }
    }

    /// Whether window `wid`'s typing reaches its terminal grid.
    pub(crate) fn typing_reaches_grid(&self, wid: WindowId) -> bool {
        matches!(self.ime_route(wid), PreeditOwner::Grid)
            && self
                .windows
                .get(&wid)
                .is_some_and(|ws| !ws.overlay_open() && ws.tab_menu.is_none())
    }

    /// Whether window `wid`'s Option keys should be an Alt
    /// ([`crate::platform::set_option_as_alt`]).
    pub(crate) fn option_is_alt(&self, wid: WindowId) -> bool {
        self.option_as_meta && (self.typing_reaches_grid(wid) || self.typing_reaches_editor(wid))
    }

    /// Whether window `wid`'s typing reaches a native Editor.
    pub(crate) fn typing_reaches_editor(&self, wid: WindowId) -> bool {
        let Some((_, view)) = self.active_native_view(wid) else {
            return false;
        };
        matches!(
            self.native_runtime.view_state(view),
            Some(crate::native_app::AppViewState::Editor(_))
        ) && self.inline_rename_edit(wid).is_none()
            && self
                .windows
                .get(&wid)
                .is_some_and(|ws| !ws.overlay_open() && ws.tab_menu.is_none())
    }

    /// Bring every window's Option keys to [`Self::option_is_alt`].
    pub(crate) fn sync_option_as_alt(&self) {
        for (wid, ws) in &self.windows {
            let alt = self.option_is_alt(*wid);
            if ws.option_as_alt.replace(alt) != alt
                && let Some(window) = ws.os_window.as_ref()
            {
                crate::platform::set_option_as_alt(window, alt);
            }
        }
    }

    /// An Option chord's press as Meta mode sends it ([`Self::on_key`]).
    fn option_as_layout_level(
        &self,
        mut ev: KeyEvent,
        mods: Option<ModifiersState>,
        option_composes: bool,
    ) -> (KeyEvent, Option<ModifiersState>) {
        let Some(m) = mods else {
            return (ev, mods);
        };
        if option_composes || !m.alt_key() || m.control_key() || m.super_key() {
            return (ev, mods);
        }
        let Some(c) = (self.option_only)(ev.physical_key, m.shift_key()) else {
            return (ev, mods);
        };
        let text = aterm_window::keyboard::SmolStr::new(c.encode_utf8(&mut [0; 4]));
        ev.logical_key = Key::Character(text.clone());
        ev.text = Some(text);
        (ev, Some(m - ModifiersState::ALT))
    }

    /// Whether an Option chord pressed in window `wid` carries what Option composed
    /// ([`crate::platform::option_composes`]).
    fn option_composes_in(&self, wid: WindowId) -> bool {
        crate::platform::option_composes(
            self.windows
                .get(&wid)
                .is_some_and(|ws| ws.option_as_alt.get()),
        )
    }

    /// Where composed text on window `wid` lands ([`Self::on_ime_commit`],
    /// [`Self::preedit_owner`]).
    fn ime_route(&self, wid: WindowId) -> PreeditOwner {
        let Some(ws) = self.windows.get(&wid) else {
            return PreeditOwner::None;
        };
        if self.active_native_view(wid).is_some() {
            return PreeditOwner::Native;
        }
        if self.inline_rename_edit(wid).is_some() {
            return PreeditOwner::Rename;
        }
        if ws.search.is_some() {
            return PreeditOwner::Find;
        }
        PreeditOwner::Grid
    }

    /// Which surface owns the in-flight IME composition on window `wid`.
    pub(crate) fn preedit_owner(&self, wid: WindowId) -> PreeditOwner {
        if self
            .windows
            .get(&wid)
            .is_none_or(|ws| ws.preedit.is_empty())
        {
            return PreeditOwner::None;
        }
        self.ime_route(wid)
    }

    /// Current keyboard modifiers as a mouse-report modifier mask.
    pub(crate) fn mouse_modifiers(&self, wid: WindowId) -> u8 {
        use aterm_types::mouse::{ALT_MASK, CTRL_MASK, SHIFT_MASK};
        let Some(mods) = self.windows.get(&wid).map(|ws| ws.mods) else {
            return 0;
        };
        let mut m = 0u8;
        if mods.shift_key() {
            m |= SHIFT_MASK;
        }
        if mods.alt_key() {
            m |= ALT_MASK;
        }
        if mods.control_key() {
            m |= CTRL_MASK;
        }
        m
    }

    /// `invoke <action>` ([`Self::dispatch_menu_action`]).
    pub(crate) fn invoke_menu_action_by_name(
        &mut self,
        el: &ActiveEventLoop,
        name: &str,
    ) -> Result<String, String> {
        let mut p = crate::palette::PaletteState::new();
        p.resolve(&self.palette_live());
        let action = p.action_by_name(name)?;
        let question_stood = self.close_banner.is_some();
        self.pending_action_refusal = None;
        self.dispatch_menu_action(el, action);
        self.invoke_reply(name, question_stood)
    }

    /// `invoke`'s reply once its action has run.
    pub(crate) fn invoke_reply(
        &mut self,
        name: &str,
        question_stood: bool,
    ) -> Result<String, String> {
        let refusal = self.pending_action_refusal.take();
        if !question_stood
            && let Some(pending) = self
                .close_banner
                .as_ref()
                .filter(|pending| pending.wire_may_answer())
        {
            return Ok(pending.wire_pending());
        }
        match refusal {
            Some(why) => Err(format!("{name}: {why}")),
            None => Ok(format!("invoked {name}")),
        }
    }

    /// Hand one picker-approved path to the capability-bounded document opener.
    fn open_local_document_path(
        &mut self,
        kind: crate::native_app::AppKind,
        path: &std::path::Path,
    ) -> Result<String, String> {
        let uri = crate::native_document_host::path_to_file_uri(path)
            .map_err(|error| format!("document open failed: {error}"))?;
        let admission = self.request_document_tab(kind, &uri)?;
        Ok(
            admission.message(&crate::app_documents::DocumentAdmissionKey {
                uri,
                kind,
                edit: None,
            }),
        )
    }

    fn choose_and_open_document(&mut self, kind: crate::native_app::AppKind) {
        let (title, prompt) = match kind {
            crate::native_app::AppKind::Markdown => ("Open Markdown", "Open as Markdown"),
            crate::native_app::AppKind::Editor => ("Open File", "Open"),
            crate::native_app::AppKind::Settings
            | crate::native_app::AppKind::SystemState
            | crate::native_app::AppKind::Recovery => return,
        };
        if self.headless {
            crate::logging::stderr_line!(
                "aterm-gui: {title} needs a window; a headless instance has no file picker"
            );
            return;
        }
        let Some(path) = menu::choose_local_file(title, prompt) else {
            return;
        };
        if let Err(error) = self.open_local_document_path(kind, &path) {
            crate::logging::stderr_line!("aterm-gui: selected document was not opened: {error}");
            menu::notify("Couldn’t Open File", &error);
        }
    }

    /// The window whose find field owns text input, if any.
    fn find_field_window(&self) -> Option<WindowId> {
        let wid = self.frontmost_window?;
        self.windows
            .get(&wid)
            .is_some_and(|ws| ws.search.is_some())
            .then_some(wid)
    }

    /// Keep a menu command from running through an open inline rename editor.
    pub(crate) fn divert_menu_action_around_rename(&mut self, action: menu::MenuAction) -> bool {
        use crate::platform::{AppRt, RenameEditorEdit};
        use menu::MenuAction;
        let Some(window) = self.front_rename_edit() else {
            return false;
        };
        if matches!(action, MenuAction::RenameSession) {
            return false;
        }
        let editing = match action {
            MenuAction::Copy => Some(RenameEditorEdit::Copy),
            MenuAction::Paste => Some(RenameEditorEdit::Paste),
            MenuAction::SelectAll => Some(RenameEditorEdit::SelectAll),
            _ => None,
        };
        if let Some(edit) = editing {
            // The in-grid field has no selection model, so Copy/Select All are inert
            // there, but still claimed.
            if self.inline_rename_edit(window).is_some() {
                if matches!(edit, RenameEditorEdit::Paste) {
                    self.rename_paste_in(window);
                }
                return true;
            }
            return self
                ._toolbars
                .get(&window)
                .is_some_and(|handle| self.apprt.rename_editor_edit(handle, edit));
        }
        self.settle_rename_edit(window);
        false
    }

    /// The licence fence ahead of a menu command, whose key equivalent never reaches `on_key`.
    pub(crate) fn fence_menu_action(&mut self, action: menu::MenuAction) {
        match self.frontmost_window {
            Some(wid) if action == menu::MenuAction::Paste => {
                self.clear_move_license_keeping_hand(wid);
            }
            _ => self.clear_front_move_license(),
        }
    }

    /// Dispatch a menu-bar click into the `App` command method the matching keybinding already
    /// uses.
    pub(crate) fn dispatch_menu_action(&mut self, el: &ActiveEventLoop, action: menu::MenuAction) {
        use menu::MenuAction;
        self.fence_menu_action(action);
        if self.divert_menu_action_around_rename(action) {
            return;
        }
        match action {
            MenuAction::About => {
                let _ = self.open_settings_tab(crate::native_settings::SettingsRoute::About);
            }
            MenuAction::Version => {
                let _ = self.open_settings_tab(crate::native_settings::SettingsRoute::About);
            }
            MenuAction::SoftwareUpdate => {
                let _ = self.open_software_update_route_and_check();
            }
            MenuAction::ApplyUpdate => self.apply_update_or_details(),
            MenuAction::Preferences => {
                if let Err(message) = self.open_config_editor_in_front() {
                    aterm_log::warn!("Open aterm.toml: {message}");
                }
            }
            MenuAction::Help => menu::open_help_url(),
            MenuAction::Quit => self.on_quit_requested(el),
            MenuAction::NewWindow => {
                self.open_window_off_loop(
                    crate::tab_spawn::SpawnAsk::default(),
                    None,
                    crate::tab_spawn::TabSpawnThen::Nothing,
                );
            }
            MenuAction::NewTab => self.open_tab(),
            MenuAction::OpenMarkdown => {
                self.choose_and_open_document(crate::native_app::AppKind::Markdown);
            }
            MenuAction::OpenEditor => {
                self.choose_and_open_document(crate::native_app::AppKind::Editor);
            }
            MenuAction::Save => {
                if let Err(why) = self.save_front_document() {
                    self.pending_action_refusal = Some(why);
                }
            }
            MenuAction::ReopenClosedTab => {
                let _ = self.reopen_last_closed_tab();
            }
            MenuAction::ReopenClosedView => {
                let _ = self.reopen_last_closed_view();
            }
            MenuAction::MoveTabToNewWindow => self.detach_active_tab(el),
            MenuAction::MoveTabToNextWindow => {
                if let Err(why) = self.migrate_active_tab_to_next_window() {
                    self.pending_action_refusal = Some(why.to_string());
                }
            }
            MenuAction::ViewSessionInNewWindow => self.open_active_session_in_new_window(el),
            MenuAction::Overview => {
                if let Err(e) = self.open_overview() {
                    self.pending_action_refusal = Some(e);
                }
            }
            MenuAction::NextNeed => {
                if let Err(why) = self.next_need() {
                    self.pending_action_refusal = Some(why);
                }
            }
            MenuAction::HarnessLog => {
                if let Some(wid) = self.frontmost_window {
                    self.open_session_ledger(wid);
                }
            }
            MenuAction::ReadThisMac
            | MenuAction::ReadFleet
            | MenuAction::FleetMachines
            | MenuAction::FabricStatus
            | MenuAction::FabricOn
            | MenuAction::FabricOff => {
                use crate::app_take_over::CliVerb;
                let verb = match action {
                    MenuAction::ReadFleet => CliVerb::ReadFleet,
                    MenuAction::FleetMachines => CliVerb::FleetPeers,
                    MenuAction::FabricStatus => CliVerb::BusStatus,
                    MenuAction::FabricOn => CliVerb::BusOn,
                    MenuAction::FabricOff => CliVerb::BusOff,
                    _ => CliVerb::ReadThisMac,
                };
                match self.frontmost_window {
                    Some(wid) => self.run_cli_to_tab(wid, verb),
                    None => {
                        self.pending_action_refusal = Some("no window to open it in".to_string());
                    }
                }
            }
            MenuAction::SummarizeFleet => {
                if let Err(why) = self.summarize_in_new_tab() {
                    self.pending_action_refusal = Some(why);
                }
            }
            MenuAction::TakeOver => {
                if let Some(wid) = self.frontmost_window {
                    self.take_over(wid);
                }
            }
            MenuAction::HandBack => {
                if let Some(wid) = self.frontmost_window {
                    self.hand_back(wid);
                }
            }
            MenuAction::SetRole => {
                if let Some(wid) = self.frontmost_window {
                    let _ = self.begin_active_session_meta_edit(
                        wid,
                        crate::session_timeline::MetaField::Role,
                    );
                }
            }
            MenuAction::ResetTerminal => {
                if let Some(wid) = self.frontmost_window {
                    self.menu_reset_terminal(wid);
                }
            }
            MenuAction::ToggleNeedsInWindows => self.user_toggle_needs_in_windows(),
            MenuAction::StatsBarsDeck | MenuAction::StatsBarsSill | MenuAction::StatsBarsOff => {
                if let Some(form) = action.stats_bars_form() {
                    self.user_set_stats_bars(form);
                }
            }
            MenuAction::DeckSizeAuto | MenuAction::DeckSizeFull | MenuAction::DeckSizeFolded => {
                if let Some(pref) = action.deck_size_pref() {
                    self.user_set_deck_size(pref);
                }
            }
            MenuAction::ToggleStatsLens => {
                let _ = self.toggle_stats_lens();
            }
            MenuAction::CloseTab => {
                // As Cmd-W; the window closes when its last tab did.
                if let Some(closed) = self.close_active_tab() {
                    self.close_window(el, closed);
                }
            }
            MenuAction::Copy => {
                let _ = self.copy_selection();
            }
            MenuAction::Paste => match self.find_field_window() {
                Some(wid) => self.search_paste_in(wid),
                None => self.paste_clipboard(),
            },
            MenuAction::CopySessionId | MenuAction::CopyCwd | MenuAction::RenameSession => {
                if let Some(window) = self.frontmost_window
                    && let Some(tab) = self
                        .windows
                        .get(&window)
                        .and_then(|ws| ws.tab_set.active_id())
                {
                    self.dispatch_tab_menu_action(el, window, tab, action);
                }
            }
            MenuAction::SelectAll => {
                if self.find_field_window().is_none() {
                    self.select_all();
                }
            }
            MenuAction::Find => self.find_requested(),
            MenuAction::FindNext | MenuAction::FindPrev => {
                let forward = matches!(action, MenuAction::FindNext);
                if let Err(why) = self.search_find_again(forward) {
                    self.pending_action_refusal = Some(why.to_string());
                }
            }
            MenuAction::ToggleFullScreen => {
                if let Err(why) = self.toggle_fullscreen() {
                    self.pending_action_refusal = Some(why.to_string());
                }
            }
            MenuAction::FontIncrease => self.set_font_size(self.font_size + FONT_ZOOM_STEP),
            MenuAction::FontDecrease => self.set_font_size(self.font_size - FONT_ZOOM_STEP),
            MenuAction::FontActualSize => self.set_font_size(self.default_font_size),
            MenuAction::SplitVertical => self.split_focused_pane(pane::SplitDir::Vertical),
            MenuAction::SplitHorizontal => self.split_focused_pane(pane::SplitDir::Horizontal),
            MenuAction::ToggleMatrixRain => {
                if let Some(wid) = self.frontmost_window {
                    self.toggle_matrix_rain(wid);
                }
            }
            MenuAction::FavouriteKitty => match self.frontmost_window {
                Some(wid) => {
                    if let Err(why) = self.favourite_kitty_checked(wid, std::time::Instant::now()) {
                        self.pending_action_refusal = Some(why);
                    }
                }
                None => self.pending_action_refusal = Some("no window to dress".to_string()),
            },
            MenuAction::NextKitty => match self.frontmost_window {
                Some(wid) => {
                    if let Err(why) = self.wear_next_kitty(wid, std::time::Instant::now()) {
                        self.pending_action_refusal = Some(why);
                    }
                }
                None => self.pending_action_refusal = Some("no window to dress".to_string()),
            },
            MenuAction::ToggleSeriousMode => {
                self.user_toggle_serious_mode();
            }
            MenuAction::ToggleSettings => {
                self.open_settings_tab(crate::native_settings::SettingsRoute::Home);
            }
            MenuAction::Packages => {
                self.open_settings_tab(crate::native_settings::SettingsRoute::Packages);
            }
            MenuAction::Messages => {
                self.open_settings_tab(crate::native_settings::SettingsRoute::Messages);
            }
            MenuAction::SystemState => self.open_system_state_from_menu(),
            MenuAction::HarnessSettings => {
                if let Some(wid) = self.frontmost_window {
                    self.open_harness_settings(wid, crate::harness_screen::HarnessFocus::Top);
                }
            }
            MenuAction::OpenPalette => self.toggle_palette(),
            MenuAction::ChooseTheme => self.open_theme_palette(),
            MenuAction::ChooseTabTheme => self.open_tab_theme_palette(),
            MenuAction::ClearTabTheme => self.clear_front_tab_theme(),
            MenuAction::BrowseThemes => {
                self.open_settings_tab(crate::native_settings::SettingsRoute::Themes);
            }
            MenuAction::NextTab => self.cycle_tab(true),
            MenuAction::PrevTab => self.cycle_tab(false),
            MenuAction::Minimize => match self.front().and_then(|ws| ws.os_window.as_ref()) {
                Some(w) => w.set_minimized(true),
                None => {
                    self.pending_action_refusal =
                        Some("no window is on screen to minimise".to_string());
                }
            },
            MenuAction::Zoom => match self.front().and_then(|ws| ws.os_window.as_ref()) {
                Some(w) => w.set_maximized(!w.is_maximized()),
                None => {
                    self.pending_action_refusal =
                        Some("no window is on screen to zoom".to_string());
                }
            },
        }
    }

    /// Dispatch one tab context-menu action against the exact tab it was popped on.
    pub(crate) fn dispatch_tab_menu_action(
        &mut self,
        el: &ActiveEventLoop,
        window: WindowId,
        tab: crate::tab_model::TabId,
        action: menu::MenuAction,
    ) {
        use menu::MenuAction;
        let Some(index) = self.tab_index_for_id(window, tab) else {
            aterm_log::info!(
                "tab context-menu {action:?} dropped: tab {tab} no longer exists in its window"
            );
            return;
        };
        match action {
            MenuAction::CopySessionId => {
                if let Some(text) = self.tab_session_id_text(window, index) {
                    let _ = crate::control::pbcopy(&text);
                }
            }
            MenuAction::CopyCwd => {
                if let Some(cwd) = self.tab_session_cwd(window, index) {
                    let _ = crate::control::pbcopy(&cwd);
                }
            }
            MenuAction::RenameSession => {
                self.begin_session_rename(window, tab);
            }
            MenuAction::CloseTab => {
                // As the `Wake::CloseTab` handler: the last tab escalates the window.
                if self.close_tab_at(window, index)
                    && let Some(ws) = self.windows.get_mut(&window)
                {
                    ws.pending_close = true;
                }
                self.escalate_pending_close(el);
            }
            MenuAction::TakeOver | MenuAction::HandBack => {
                let Some(session) = self.tab_terminal_session(window, index) else {
                    aterm_log::info!("{action:?} dropped: the tab has no session");
                    return;
                };
                let done = if action == MenuAction::TakeOver {
                    self.take_over_session(session)
                } else {
                    self.hand_back_session(session)
                };
                if let Err(why) = done {
                    self.pending_action_refusal = Some(why);
                }
            }
            MenuAction::HarnessLog => {
                if let Some(session) = self.tab_terminal_session(window, index) {
                    self.open_session_ledger_for(window, session);
                }
            }
            MenuAction::SetRole => {
                let _ = self.begin_session_meta_edit(
                    window,
                    tab,
                    crate::session_timeline::MetaField::Role,
                );
            }
            other => self.dispatch_menu_action(el, other),
        }
    }

    /// Dispatch one session-argument row of a tab's context menu.
    pub(crate) fn dispatch_tab_menu_connection(
        &mut self,
        window: WindowId,
        tab: crate::tab_model::TabId,
        peer_sid: &aterm_session::SessionId,
        verb: crate::session_chrome::ConnVerb,
    ) {
        use crate::platform::AppRt as _;
        use crate::session_chrome::ConnVerb;
        if self.tab_index_for_id(window, tab).is_none() {
            aterm_log::info!("tab menu {verb:?} dropped: tab {tab} no longer exists in its window");
            return;
        }
        let local = {
            let g = self.store.read().unwrap_or_else(|p| p.into_inner());
            g.by_sid(peer_sid).map(|h| h.local_id)
        };
        let Some(local) = local else {
            aterm_log::info!("tab menu {verb:?} {}: no such session", peer_sid.as_str());
            return;
        };
        match verb {
            ConnVerb::Show => {
                if let Err(e) = self.raise_session_by_id(local) {
                    aterm_log::info!("show {}: {e}", peer_sid.as_str());
                }
            }
            ConnVerb::StopWorker => {
                if let Some(why) = self.stop_worker_refusal(local) {
                    self.stop_worker_refused(local, why);
                    return;
                }
                if self.person_gesture_refused() {
                    match self.stop_worker(local) {
                        Ok(reply) => aterm_log::info!(
                            "stop worker {local}: a control client's gesture: {reply}"
                        ),
                        Err(why) => self.stop_worker_refused(local, why),
                    }
                    return;
                }
                let label = self.tab_label_of_session(local);
                let body =
                    format!("Stop {label}? Its agent gets SIGTERM and its task reads exited.");
                let real_window = self
                    .windows
                    .get(&window)
                    .is_some_and(|ws| ws.os_window.is_some());
                let answer = if real_window {
                    self.apprt.confirm_owner("Stop Worker", &body, "Stop")
                } else {
                    None
                };
                #[cfg(test)]
                let answer = self.agents.owner_sheet_for_test.unwrap_or(answer);
                match answer {
                    Some(true) => {
                        if let Err(why) = self.stop_worker(local) {
                            self.stop_worker_refused(local, why);
                        }
                    }
                    Some(false) => {}
                    None => self.overview_arm_stop(window, local),
                }
            }
        }
    }

    /// The registry `sid` string for the terminal session labeling tab `index` of `window`.
    pub(crate) fn tab_session_id_text(&self, window: WindowId, index: usize) -> Option<String> {
        let session = self.tab_terminal_session(window, index)?;
        self.store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_local(session)
            .map(|h| h.sid.as_str().to_string())
    }

    /// Raw shell-reported cwd of the terminal session for tab `index` of `window`, or `None`.
    pub(crate) fn tab_session_cwd(&self, window: WindowId, index: usize) -> Option<String> {
        use crate::cwd_native::ReportedCwd as _;
        let session = self.tab_terminal_session(window, index)?;
        let s = self.pool.get(session)?;
        let term = crate::term_lock(&s.term);
        term.native_working_directory()
            .filter(|c| !c.is_empty())
            .map(|c| c.into_owned())
    }

    /// The current position of the tab with stable id `tab` in `window`.
    pub(crate) fn tab_index_for_id(
        &self,
        window: WindowId,
        tab: crate::tab_model::TabId,
    ) -> Option<usize> {
        let ws = self.windows.get(&window)?;
        ws.tab_set.tabs().iter().position(|t| t.id == tab)
    }

    /// The terminal session labeling tab `index` of `window`.
    pub(crate) fn tab_terminal_session(&self, window: WindowId, index: usize) -> Option<u64> {
        let ws = self.windows.get(&window)?;
        let tab = ws.tab_set.tabs().get(index)?;
        self.view_store
            .get(tab.focus)
            .copied()
            .and_then(crate::tab_model::View::terminal_session)
    }

    /// Request a Serious Mode transition through the versioned config lane native Settings uses.
    pub(crate) fn user_toggle_serious_mode(&mut self) {
        if let Err(error) = self.queue_serious_mode_toggle() {
            self.post_message(crate::message_reporters::serious_mode_feedback(&format!(
                "Serious Mode was not changed: {error}"
            )));
        }
    }

    /// Open a compatibility GUI target through the menu items' paths.
    pub(crate) fn open_aux_window(
        &mut self,
        _el: &aterm_window::event_loop::ActiveEventLoop,
        target: crate::app_introspect::AuxTarget,
    ) -> Result<(), String> {
        use crate::app_introspect::AuxTarget;
        if target == AuxTarget::Update {
            return self.open_software_update_route_and_check().map(|_| ());
        }
        if let Some(route) = native_settings_route_for_aux(target) {
            return if self.open_settings_tab(route) {
                Ok(())
            } else {
                Err(format!(
                    "could not open the native Settings {} route",
                    route.path()
                ))
            };
        }
        match target {
            AuxTarget::Menu => {
                self.palette_enter();
                Ok(())
            }
            AuxTarget::TabMenu => {
                let Some(wid) = self.frontmost_window else {
                    return Err("no front window to open the tab menu on".to_string());
                };
                if self.open_active_tab_context_menu(wid) {
                    Ok(())
                } else {
                    Err("the active tab has no session context menu".to_string())
                }
            }
            AuxTarget::Overview => self.open_overview(),
            AuxTarget::AutopilotMenu => self.open_front_autopilot_menu(),
            AuxTarget::FactMenu => {
                use crate::claude_lights::Fact;
                let names: Vec<&str> = Fact::EVERY.into_iter().map(Fact::key).collect();
                Err(format!(
                    "open fact-menu names its fact: {}",
                    names.join(" | ")
                ))
            }
            AuxTarget::Front => {
                Err("the front terminal window is already open (use window/chrome)".to_string())
            }
            AuxTarget::Prefs | AuxTarget::About | AuxTarget::Update => {
                Err("native Settings alias routing failed".to_string())
            }
        }
    }

    /// The `open <target> close` counterpart.
    pub(crate) fn close_aux_overlay(
        &mut self,
        target: crate::app_introspect::AuxTarget,
    ) -> Result<(), String> {
        use crate::app_introspect::AuxTarget;
        match target {
            AuxTarget::Prefs | AuxTarget::About | AuxTarget::Update => {
                self.close_settings_tabs();
                Ok(())
            }
            AuxTarget::Menu => {
                self.palette_exit();
                Ok(())
            }
            AuxTarget::TabMenu => {
                if let Some(wid) = self.frontmost_window {
                    self.close_tab_menu(wid);
                }
                Ok(())
            }
            AuxTarget::AutopilotMenu => {
                self.close_autopilot_menu();
                Ok(())
            }
            AuxTarget::FactMenu => {
                self.close_fact_menu();
                Ok(())
            }
            AuxTarget::Overview => {
                if let Some(wid) = self.frontmost_window {
                    self.overview_exit(wid);
                }
                Ok(())
            }
            AuxTarget::Front => Err(
                "close supports native Settings routes (prefs | about | update) and menu"
                    .to_string(),
            ),
        }
    }
}

#[cfg(all(test, unix))]
mod forwarded_release_handoff_activity_tests {
    use crate::{App, WindowId};
    use aterm_window::keyboard::{KeyCode, PhysicalKey};

    fn arm_pending_overlap(app: &mut App) -> std::sync::mpsc::Receiver<()> {
        let (cancel, cancelled) = std::sync::mpsc::sync_channel(1);
        app.pending_update_handoff = Some(crate::PendingUpdateHandoff {
            park_at: std::time::Instant::now(),
            proof_ready_at: None,
            attempt_id: 1,
            nonce: None,
            live: Vec::new(),
            adoption: Vec::new(),
            child_pid: None,
            mode: crate::native_updater_service::ApplyMode::Immediate,
            apply_attempt: None,
            same_image: None,
            target_build: 0,
            target_commit: String::new(),
            layout: crate::restore::RestoreManifest::new(Vec::new()),
            layout_digest: [0; 32],
            screen_digest: [0; 32],
            activity_epoch: app.update_handoff_activity_epoch,
            hold_serials: 0,
            carried_claims: Vec::new(),
            cancel,
            arbiter: crate::HandoffAttemptArbiter::new(),
            teardown: crate::DeferredHandoffTeardown::None,
            commit_drain_started: None,
            commit_flush: None,
            held_commit: None,
            revoked_by_activity: false,
        });
        cancelled
    }

    #[test]
    fn forwarded_release_is_tolerated_while_overlap_pending() {
        use aterm_types::keyboard::{Key as TKey, Modifiers};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let physical = PhysicalKey::Code(KeyCode::KeyA);
        app.note_forwarded_press_key(
            wid,
            physical,
            false,
            TKey::Character('a'),
            Modifiers::empty(),
            None,
            None,
        );
        let cancelled = arm_pending_overlap(&mut app);
        let epoch = app.update_handoff_activity_epoch;
        app.release_physical_press(wid, physical);
        assert!(
            !app.physical_press_owners.contains_key(&physical),
            "the forwarded episode is closed by its release"
        );
        assert_eq!(
            app.update_handoff_activity_epoch, epoch,
            "a key release must not bump the activity epoch while an overlap is pending"
        );
        assert!(
            cancelled.try_recv().is_err(),
            "a key release must not poke the pending overlap's cancel channel"
        );
    }
}

#[cfg(test)]
mod native_aux_alias_tests {
    use super::native_settings_route_for_aux;
    use crate::app_introspect::AuxTarget;
    use crate::native_app::AppViewState;
    use crate::native_settings::SettingsRoute;
    use crate::{App, WindowId};

    #[test]
    fn legacy_about_and_update_spellings_navigate_native_settings_routes() {
        for (spelling, route) in [
            ("about", SettingsRoute::About),
            ("update", SettingsRoute::SoftwareUpdate),
            ("software-update", SettingsRoute::SoftwareUpdate),
        ] {
            let target = AuxTarget::parse(spelling).expect("compatibility spelling");
            assert_eq!(native_settings_route_for_aux(target), Some(route));

            let mut app = App::headless_for_test();
            assert!(app.open_settings_tab(route));
            let (_, view) = app
                .active_native_view(WindowId(0))
                .expect("native Settings view");
            assert!(matches!(
                app.native_runtime.view_state(view),
                Some(AppViewState::Settings(state)) if state.route == route
            ));
            assert!(app.windows[&WindowId(0)].overlay.is_none());
            assert!(app.close_settings_tabs());
            assert!(app.active_native_view(WindowId(0)).is_none());
        }
    }
}

#[cfg(test)]
mod native_preferences_menu_tests {
    use crate::native_app::AppKind;
    use crate::{App, WindowId};

    #[test]
    fn open_aterm_toml_menu_command_is_the_native_manual_editor() {
        const ROOT: &str = "ATERM_NATIVE_PREFERENCES_MENU_ROOT";
        if !crate::test_alone::in_child() {
            let root = aterm_tempfile::TempDir::new().unwrap();
            crate::test_alone::run_alone_with(&[
                (ROOT, root.path().as_os_str()),
                ("XDG_CONFIG_HOME", root.path().as_os_str()),
            ]);
            return;
        }

        let root = std::path::PathBuf::from(std::env::var_os(ROOT).unwrap());
        let mut app = App::headless_for_test();
        app.open_config_editor_in_front()
            .expect("the menu command opens aterm.toml");
        assert!(
            root.join("aterm/aterm.toml").is_file(),
            "Manual must resolve only the isolated config file"
        );
        let (instance, _) = app
            .active_native_view(WindowId(0))
            .expect("native Manual editor");
        assert_eq!(
            app.native_runtime.app(instance).map(|app| app.kind()),
            Some(AppKind::Editor)
        );
        assert!(app.windows[&WindowId(0)].front_terminal().is_none());
        assert!(app.windows[&WindowId(0)].overlay.is_none());
    }
}

#[cfg(test)]
mod serious_mode_command_tests {
    use crate::App;

    #[test]
    fn unavailable_config_lane_leaves_runtime_and_service_unchanged_and_reports_failure() {
        let mut app = App::headless_for_test();
        let before = app.native_config_service.snapshot();
        let rows_before = app.messages.live_rows().count();
        assert!(!app.serious_mode_enabled());
        assert_eq!(app.config.serious_mode, None);
        assert!(
            app.proxy.is_none(),
            "negative control needs no event-loop host"
        );

        app.user_toggle_serious_mode();

        assert!(
            !app.serious_mode_enabled(),
            "the live suppression policy must not move before durability"
        );
        assert_eq!(app.config.serious_mode, None);
        let after = app.native_config_service.snapshot();
        assert_eq!(after.revision, before.revision);
        assert_eq!(after.text, before.text);
        assert!(
            app.native_config_pending.is_empty(),
            "a failed preflight must not leave a command that can apply later"
        );
        assert!(
            app.messages.live_rows().count() > rows_before,
            "Finder-launched users need visible failure feedback"
        );
    }
}

/// `[keybindings]` actions for commands that are otherwise menu items.
#[cfg(test)]
mod menu_only_action_dispatch_tests {
    use crate::keybinding::Action;
    use crate::{App, WindowId, term_lock};

    fn app_with_three_matches() -> (App, WindowId) {
        let app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.pool.get(0).expect("session 0").term.clone();
        term_lock(&term).process(b"hit one\r\nhit two\r\nhit three");
        (app, wid)
    }

    fn current(app: &App, wid: WindowId) -> usize {
        app.windows[&wid]
            .search
            .as_ref()
            .expect("open find bar")
            .current
    }

    #[test]
    fn find_again_actions_step_an_open_bar_and_resume_a_closed_one() {
        let (mut app, wid) = app_with_three_matches();
        app.search_enter();
        app.windows
            .get_mut(&wid)
            .unwrap()
            .search
            .as_mut()
            .unwrap()
            .query = "hit".into();
        app.search_recompute();
        let first = current(&app, wid);

        app.dispatch_action(wid, Action::FindNext);
        let second = current(&app, wid);
        assert_ne!(second, first, "find_next stepped the open bar");
        app.dispatch_action(wid, Action::FindPrev);
        assert_eq!(current(&app, wid), first, "find_prev stepped back");

        app.search_accept();
        assert!(
            app.windows[&wid].search.is_none(),
            "Enter closed the bar and remembered the query"
        );
        app.dispatch_action(wid, Action::FindNext);
        assert_eq!(
            app.windows[&wid]
                .search
                .as_ref()
                .expect("find_next reopened the accepted query")
                .query,
            "hit",
        );
    }

    #[test]
    fn select_all_action_selects_the_screen_and_defers_to_an_open_find() {
        let (mut app, wid) = app_with_three_matches();
        let term = app.pool.get(0).expect("session 0").term.clone();
        assert!(
            term_lock(&term).selection_to_string().is_none(),
            "negative control: nothing selected yet"
        );
        app.dispatch_action(wid, Action::SelectAll);
        let all = term_lock(&term)
            .selection_to_string()
            .expect("select_all selected the screen");
        assert!(all.contains("hit three"), "{all:?}");

        app.search_enter();
        app.windows
            .get_mut(&wid)
            .unwrap()
            .search
            .as_mut()
            .unwrap()
            .query = "hit one".into();
        app.search_recompute();
        let marked = term_lock(&term)
            .selection_to_string()
            .expect("the find marked its match");
        app.dispatch_action(wid, Action::SelectAll);
        assert_eq!(
            term_lock(&term).selection_to_string(),
            Some(marked),
            "select_all must not eat the find bar's match highlight",
        );
    }
}

#[cfg(test)]
#[path = "app_input_native_keyboard_boundary_tests.rs"]
mod native_keyboard_boundary_tests;

#[cfg(test)]
mod local_repeat_behavior_tests {
    use crate::{App, LocalRepeatAction, SearchRepeatAction, WindowId, term_lock};
    use aterm_window::keyboard::{KeyCode, PhysicalKey};

    #[test]
    fn search_repeat_stays_on_press_window_after_focus_changes() {
        let mut app = App::headless_for_test();
        let press_window = WindowId(0);
        let terminal = app
            .front_terminal(press_window)
            .expect("press terminal")
            .term
            .clone();
        term_lock(&terminal).process(b"focus_routing_needle");
        app.search_enter();

        let session = app
            .front_terminal(press_window)
            .expect("press terminal")
            .session;
        let physical = PhysicalKey::Code(KeyCode::KeyQ);
        app.note_local_repeat_press(
            press_window,
            physical,
            LocalRepeatAction::Search {
                session,
                action: SearchRepeatAction::Edit(crate::app_search::SearchEdit::Insert(
                    "focus_routing_needle".to_string(),
                )),
            },
        );

        let next_session = app.next_session_id;
        let arrival_window = app.insert_logical_window(crate::stub_session(next_session), 24, 80);
        assert_eq!(app.frontmost_window, Some(arrival_window));

        app.route_physical_repeat(arrival_window, physical);

        let search = app.windows[&press_window]
            .search
            .as_ref()
            .expect("press-window search remains open");
        assert_eq!(search.query, "focus_routing_needle");
        assert!(
            !search.matches.is_empty(),
            "repeat edit must recompute and navigate its press-time window"
        );
        assert!(app.windows[&arrival_window].search.is_none());
        assert!(app.take_local_repeat_release(arrival_window, physical));
    }

    #[test]
    fn palette_repeat_stops_when_its_press_time_overlay_closes() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.palette_enter();
        let physical = PhysicalKey::Code(KeyCode::KeyN);
        app.note_local_repeat_press(
            wid,
            physical,
            LocalRepeatAction::Palette(crate::app_palette::PaletteKey::Edit(
                crate::app_search::SearchEdit::Insert("n".to_string()),
            )),
        );
        app.route_physical_repeat(wid, physical);
        let before_close = app.windows[&wid]
            .palette()
            .expect("palette")
            .controls_lines();
        assert!(before_close[0].contains("query=\"n\""), "{before_close:?}");

        app.palette_exit();
        app.route_physical_repeat(wid, physical);
        assert!(app.windows[&wid].palette().is_none());
        assert!(app.take_local_repeat_release(wid, physical));
    }
}

#[cfg(test)]
mod ledger_key_tests {
    //! Keyboard, IME, mouse-modifier, and action dispatch methods on `App`.

    use crate::keybinding::Action;
    use crate::{App, WindowId};
    use aterm_window::event::{ElementState, KeyEvent};
    use aterm_window::keyboard::{Key, KeyCode, KeyLocation, ModifiersState, PhysicalKey, SmolStr};

    fn shift_l() -> KeyEvent {
        KeyEvent::synthetic_for_test(
            PhysicalKey::Code(KeyCode::KeyL),
            Key::Character(SmolStr::new("L")),
            None,
            KeyLocation::Standard,
            ElementState::Pressed,
            false,
        )
    }

    #[test]
    fn cmd_shift_l_composes_the_focused_sessions_ledger_command() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let sid = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(sid));
        app.frontmost_window = Some(wid);
        let stable = app
            .pool
            .get(sid)
            .expect("stub pooled")
            .ctx
            .self_id
            .as_str()
            .to_string();
        assert!(app.last_ledger_plan.is_none());

        let fired =
            app.on_key_super_shift_chord(ModifiersState::SUPER | ModifiersState::SHIFT, &shift_l());
        assert!(fired, "⇧⌘L is consumed, never typed into the PTY");
        let plan = app.last_ledger_plan.clone().expect("a plan was composed");
        assert_eq!(plan.sid, stable);
        assert_eq!(plan.argv[..2], ["drive", "ledger"]);
        assert_eq!(plan.argv[2], format!("@{stable}"));
        assert_eq!(plan.argv[3..6], ["--format", "html", "--out"]);
        assert_eq!(plan.argv[6], plan.out.to_string_lossy());

        app.last_ledger_plan = None;
        assert!(!app.on_key_super_shift_chord(ModifiersState::SUPER, &shift_l()));
        assert!(app.last_ledger_plan.is_none());

        app.dispatch_action(wid, Action::OpenLedger);
        assert_eq!(
            app.last_ledger_plan.as_ref().map(|p| p.sid.as_str()),
            Some(stable.as_str())
        );
    }
}

#[cfg(test)]
#[path = "app_input_terminal_emacs_search_input_tests.rs"]
mod terminal_emacs_search_input_tests;

/// The seam classifies a keypad press as its main-block twin.
#[cfg(test)]
mod keypad_press_class_tests {
    use super::{OutputEchoInput, PetInputKind, PredictKey, classify_press, is_plain_enter};
    use crate::input::InputEvent;
    use crate::predict::EditKey;
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

    fn press(key: NamedKey, mods: Modifiers) -> InputEvent {
        InputEvent::Key {
            key: Key::Named(key),
            mods,
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        }
    }

    #[test]
    fn keypad_enter_is_a_plain_enter_and_a_turn_boundary() {
        let kp_enter = press(NamedKey::NumpadEnter, Modifiers::empty());
        assert!(is_plain_enter(&kp_enter));
        assert_eq!(
            OutputEchoInput::of(&kp_enter),
            OutputEchoInput::TurnBoundary
        );
        let class = classify_press(&kp_enter);
        assert!(class.enter_like && class.keyed_enter);
        assert!(matches!(class.console_input, Some(PetInputKind::Submit)));
        assert_eq!(class.typed_forward, Some(true));
        let shifted = press(NamedKey::NumpadEnter, Modifiers::SHIFT);
        assert!(!is_plain_enter(&shifted));
        assert!(classify_press(&shifted).enter_like);
    }

    #[test]
    fn keypad_digit_is_a_typed_glyph() {
        let five_ev = press(NamedKey::Numpad5, Modifiers::empty());
        let class = classify_press(&five_ev);
        assert_eq!(class.typed, Some('5'));
        assert_eq!(class.predict_candidate, Some(PredictKey::Glyph('5')));
        assert_eq!(class.typed_forward, Some(true));
        assert!(matches!(class.console_input, Some(PetInputKind::Text)));
        assert!(!class.brk);
        let decimal_ev = press(NamedKey::NumpadDecimal, Modifiers::empty());
        let decimal = classify_press(&decimal_ev);
        assert_eq!(decimal.typed, Some('.'));
        let shifted_ev = press(NamedKey::Numpad5, Modifiers::SHIFT);
        let shifted = classify_press(&shifted_ev);
        assert_eq!(shifted.typed, Some('5'));
        assert_eq!(shifted.predict_candidate, Some(PredictKey::Glyph('5')));
        let equal_ev = press(NamedKey::NumpadEqual, Modifiers::SHIFT);
        assert_eq!(classify_press(&equal_ev).typed, Some('='));
    }

    #[test]
    fn only_bare_arrows_and_backspace_are_edit_keys() {
        let candidate = |key, mods| classify_press(&press(key, mods)).predict_candidate;
        for locks in [
            Modifiers::CAPS_LOCK,
            Modifiers::NUM_LOCK,
            Modifiers::CAPS_LOCK | Modifiers::NUM_LOCK,
        ] {
            assert_eq!(
                candidate(NamedKey::ArrowRight, locks),
                Some(PredictKey::Edit(EditKey::Right)),
                "{locks:?}"
            );
            assert_eq!(
                candidate(NamedKey::ArrowLeft, locks | Modifiers::ALT),
                None,
                "{locks:?} with Alt is a word motion"
            );
        }
        for mods in [Modifiers::HYPER, Modifiers::META] {
            assert_eq!(candidate(NamedKey::ArrowLeft, mods), None, "{mods:?}");
        }
        assert_eq!(
            candidate(NamedKey::ArrowLeft, Modifiers::empty()),
            Some(PredictKey::Edit(EditKey::Left))
        );
        assert_eq!(
            candidate(NamedKey::ArrowRight, Modifiers::empty()),
            Some(PredictKey::Edit(EditKey::Right))
        );
        assert_eq!(
            candidate(NamedKey::Backspace, Modifiers::empty()),
            Some(PredictKey::Edit(EditKey::Backspace))
        );
        for mods in [
            Modifiers::SHIFT,
            Modifiers::CTRL,
            Modifiers::ALT,
            Modifiers::SUPER,
        ] {
            assert_eq!(candidate(NamedKey::ArrowLeft, mods), None, "{mods:?}");
        }
        for locks in [Modifiers::empty(), Modifiers::NUM_LOCK] {
            assert_eq!(
                candidate(NamedKey::NumpadArrowLeft, locks),
                Some(PredictKey::Edit(EditKey::Left)),
                "the keypad's ←, {locks:?}"
            );
            assert_eq!(
                candidate(NamedKey::NumpadArrowRight, locks),
                Some(PredictKey::Edit(EditKey::Right)),
                "the keypad's →, {locks:?}"
            );
        }
        assert_eq!(
            candidate(NamedKey::NumpadArrowLeft, Modifiers::SHIFT),
            None,
            "a chord on the keypad is a chord"
        );
        assert_eq!(candidate(NamedKey::ArrowUp, Modifiers::empty()), None);
    }

    #[test]
    fn home_end_delete_and_ctrl_a_e_are_learned_edit_keys() {
        let candidate = |key, mods| classify_press(&press(key, mods)).predict_candidate;
        for (key, edit) in [
            (NamedKey::Home, EditKey::Home),
            (NamedKey::End, EditKey::End),
            (NamedKey::Delete, EditKey::Delete),
        ] {
            for locks in [
                Modifiers::empty(),
                Modifiers::CAPS_LOCK | Modifiers::NUM_LOCK,
            ] {
                assert_eq!(
                    candidate(key, locks),
                    Some(PredictKey::Edit(edit)),
                    "{key:?} {locks:?}"
                );
            }
            for mods in [
                Modifiers::SHIFT,
                Modifiers::ALT,
                Modifiers::SUPER,
                Modifiers::CTRL,
            ] {
                assert_eq!(candidate(key, mods), None, "{key:?} {mods:?}");
            }
        }
        for key in [
            NamedKey::NumpadHome,
            NamedKey::NumpadEnd,
            NamedKey::NumpadDelete,
        ] {
            assert_eq!(candidate(key, Modifiers::empty()), None, "{key:?}");
        }
        let chord = |c: char, mods| {
            classify_press(&InputEvent::Key {
                key: Key::Character(c),
                mods,
                base_layout: None,
                layout_text: None,
                event_type: KeyEventType::Press,
            })
            .predict_candidate
        };
        for (c, edit) in [('a', EditKey::CtrlA), ('e', EditKey::CtrlE)] {
            assert_eq!(
                chord(c, Modifiers::CTRL),
                Some(PredictKey::Edit(edit)),
                "Ctrl-{c}"
            );
            assert_eq!(
                chord(c, Modifiers::CTRL | Modifiers::NUM_LOCK),
                Some(PredictKey::Edit(edit)),
                "Ctrl-{c} with a lock"
            );
            for mods in [
                Modifiers::CTRL | Modifiers::SHIFT,
                Modifiers::CTRL | Modifiers::ALT,
                Modifiers::CTRL | Modifiers::SUPER,
            ] {
                assert_eq!(chord(c, mods), None, "{c} {mods:?}");
            }
            assert_eq!(chord(c, Modifiers::empty()), Some(PredictKey::Glyph(c)));
        }
        assert_eq!(chord('b', Modifiers::CTRL), None, "Ctrl-B is not modelled");
    }

    #[test]
    fn keypad_navigation_and_delete_classify_as_their_twins() {
        let left_ev = press(NamedKey::NumpadArrowLeft, Modifiers::empty());
        let left = classify_press(&left_ev);
        assert!(left.navigation_key && left.brk);
        assert!(matches!(left.console_input, Some(PetInputKind::Navigate)));
        let end_ev = press(NamedKey::NumpadEnd, Modifiers::empty());
        assert!(classify_press(&end_ev).navigation_key);
        let del_ev = press(NamedKey::NumpadDelete, Modifiers::empty());
        let del = classify_press(&del_ev);
        assert!(del.kill_key && !del.kill_moves && del.kill_word);
        assert!(matches!(del.console_input, Some(PetInputKind::Delete)));
    }

    #[test]
    fn keypad_begin_and_the_main_block_are_untouched() {
        let begin_ev = press(NamedKey::NumpadBegin, Modifiers::empty());
        let begin = classify_press(&begin_ev);
        assert_eq!(begin.typed, None);
        assert!(!begin.navigation_key && !begin.enter_like && !begin.kill_key);
        assert!(begin.console_input.is_none());
        let five = InputEvent::Key {
            key: Key::Character('5'),
            mods: Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        };
        assert_eq!(classify_press(&five).typed, Some('5'));
        assert!(is_plain_enter(&press(NamedKey::Enter, Modifiers::empty())));
    }
}

#[cfg(test)]
mod mouse_cell_clobber_tests {
    use crate::App;
    use crate::WindowId;
    use crate::input::{InputEvent, PixelOffset, Source};
    use aterm_core::selection::SelectionSide;

    #[test]
    fn mouse_move_does_not_clobber_pane_local_cell() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.windows.get_mut(&wid).unwrap().last_mouse_cell = (5, 7);
        let _ = app.input(
            wid,
            InputEvent::MouseMove {
                buttons: 3,
                row: 30,
                col: 30,
                mods: 0,
                side: SelectionSide::Left,
                px_off: PixelOffset::CELL_ORIGIN,
            },
            Source::Human,
        );
        assert_eq!(
            app.windows.get(&wid).unwrap().last_mouse_cell,
            (5, 7),
            "the seam must not overwrite the pane-local last_mouse_cell"
        );
    }
}

#[cfg(test)]
#[path = "app_input_kitty_orphan_release_tests.rs"]
mod kitty_orphan_release_tests;

/// Conformance test conformance for the process-wide physical press owner.
#[cfg(test)]
#[path = "app_input_input_release_pairing_conformance_tests.rs"]
pub(crate) mod input_release_pairing_conformance;

/// On every route the host drives (`paste_order::routes_for_test`).
#[cfg(test)]
#[path = "app_input_overlay_gate_pairing_tests.rs"]
mod overlay_gate_pairing_tests;

#[cfg(test)]
#[path = "app_input_smooth_scroll_tests.rs"]
mod smooth_scroll_tests;

#[cfg(test)]
#[path = "app_input_keystroke_press_side_effect_tests.rs"]
mod keystroke_press_side_effect_tests;

/// Lock-elision guards on the press/release path.
#[cfg(test)]
#[path = "app_input_press_path_lock_elision_tests.rs"]
mod press_path_lock_elision_tests;

/// The layout's character on a text press (Caps Lock, Shift on a non-us layout) and Ctrl on a
/// non-Latin one.
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "app_input_layout_text_tests.rs"]
mod layout_text_tests;

#[cfg(test)]
#[path = "app_input_option_as_alt_tests.rs"]
mod option_as_alt_tests;

#[cfg(all(test, unix))]
#[path = "app_input_pet_console_input_tests.rs"]
mod pet_console_input_tests;

#[cfg(all(test, unix))]
#[path = "app_input_pane_focus_key_tests.rs"]
mod pane_focus_key_tests;

#[cfg(all(test, unix))]
#[path = "app_input_paste_cursor_gesture_tests.rs"]
mod paste_cursor_gesture_tests;

#[cfg(test)]
mod predictive_echo_input_gate_tests {
    use std::time::{Duration, Instant};

    use crate::input::{InputEvent, Source};
    use crate::{App, WindowId, term_lock};
    use aterm_predict::PredictMode;
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers};

    fn printable(ch: char) -> InputEvent {
        InputEvent::Key {
            key: Key::Character(ch),
            mods: Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        }
    }

    #[test]
    fn codex_report_event_types_never_arms_the_expiry_erase() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();

        let model = aterm_models_core::predictive_echo_visibility_model();
        let mut model_state = model.init_state();

        app.config.predictive_echo = Some("adaptive".to_string());
        let now = Instant::now();
        let first_confirmed = now + Duration::from_millis(60);
        let second_confirmed = first_confirmed + Duration::from_millis(60);
        {
            let predictor = &mut app.windows.get_mut(&wid).expect("window").predictor;
            predictor.set_mode(PredictMode::Adaptive);
            assert!(predictor.predict_char('a', (0, 0), 0, 80, now));
            predictor.reconcile(Some((0, 1)), 0, false, first_confirmed, |row, col| {
                ((row, col) == (0, 0)).then_some('a')
            });
            assert!(predictor.predict_char('b', (0, 1), 0, 80, first_confirmed));
            predictor.reconcile(Some((0, 2)), 0, false, second_confirmed, |row, col| {
                ((row, col) == (0, 1)).then_some('b')
            });
            assert!(predictor.predict_char('c', (0, 2), 0, 80, second_confirmed));
            assert!(predictor.is_displaying(second_confirmed));
        }
        assert!(model.fire("ConfirmSlow", &mut model_state));
        assert!(model.fire("Key", &mut model_state));
        assert_eq!(model_state["pending"], 1);
        assert_eq!(model_state["visible"], 1);

        assert!(
            app.windows[&wid].predictor.next_deadline().is_some(),
            "control: the pre-Codex visible guess armed the 250 ms self-heal"
        );

        term_lock(&term).process(b"\x1b[>7u");
        assert!(model.fire("EnterComposer", &mut model_state));
        crate::app_input::pin_input_clock(Some(second_confirmed));
        let _ = app.input(wid, printable('c'), Source::Human);
        assert!(model.fire("Key", &mut model_state));
        let predictor = &app.windows[&wid].predictor;
        assert!(
            predictor.idle(),
            "the app-owned composer flushes prior guesses"
        );
        assert_eq!(model_state["pending"], 0);
        assert_eq!(model_state["visible"], 0);
        assert!(
            predictor.next_deadline().is_none(),
            "Codex input must not arm a delayed erase"
        );
    }
}

/// Typed-word summon plumbing (the detector's laws are proven in `aterm_effects::kitty_summon`).
#[cfg(test)]
#[path = "app_input_typed_kitty_summon_tests.rs"]
mod typed_kitty_summon_tests;

/// Predictive echo at prompts that do not echo, end to end through the press path and the composed
/// present.
#[cfg(test)]
#[path = "app_input_predict_privacy_tests.rs"]
mod predict_privacy_tests;

/// Kitty commands: host wiring of the typed-line listener (`aterm_effects::typed_tricks`).
#[cfg(test)]
#[path = "app_input_typed_trick_tests.rs"]
mod typed_trick_tests;

/// Favourite-this-kitty App seams (the ledger laws are proven in `kitty_log`).
#[cfg(test)]
#[path = "app_input_favourite_kitty_tests.rs"]
mod favourite_kitty_tests;

/// The homecoming rate law's App half (the gate's laws are proven in
/// `aterm_effects::companion_look`).
#[cfg(test)]
#[path = "app_input_homecoming_rate_law_tests.rs"]
mod homecoming_rate_law_tests;

/// Keyboard parity for the tab context menu through the shipping `on_key` with real winit events.
#[cfg(all(test, any(windows, target_os = "linux")))]
#[path = "app_input_tab_menu_key_tests.rs"]
mod tab_menu_key_tests;

/// The control seam (`ctl key menu`, `ctl key down`, `ctl mouse wheel`) pops.
#[cfg(all(test, any(windows, target_os = "linux")))]
#[path = "app_input_tab_menu_seam_tests.rs"]
mod tab_menu_seam_tests;

/// The find field through the shipping `on_key` with real winit events.
#[cfg(test)]
#[path = "app_input_find_field_key_tests.rs"]
mod find_field_key_tests;

/// A pane-focus chord on an arrow claims its key only while a pane lies that way.
#[cfg(test)]
#[path = "app_input_pane_focus_fall_through_tests.rs"]
mod pane_focus_fall_through_tests;

/// The window's key layers on real winit events.
#[cfg(test)]
#[path = "app_input_key_layer_tests.rs"]
mod key_layer_tests;

/// The `tone` control verb's App half ([`App::tone_status`]): the readout that makes an
/// audible-only feature diagnosable.
#[cfg(test)]
mod tone_status_tests {
    use crate::input::{InputEvent, Source};
    use crate::trail_audio::TrailAudio;
    use crate::{App, WindowId};
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers};

    fn key(character: char) -> InputEvent {
        InputEvent::Key {
            key: Key::Character(character),
            mods: Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        }
    }

    fn type_line(app: &mut App, wid: WindowId, line: &str) {
        for ch in line.chars() {
            let _ = app.input(wid, key(ch), Source::Human);
        }
    }

    #[test]
    fn a_muted_build_reports_the_gate_that_stopped_the_classifier() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(!app.trail_audio.is_live(), "the headless host is sealed");
        type_line(&mut app, wid, "why is this broken again ugh");

        let line = app.tone_status().expect("the fixture window is focused");
        assert!(line.contains("audio=inert"), "{line}");
        assert!(line.contains("active=false"), "{line}");
        assert!(line.contains("inferences=0"), "{line}");
        assert!(line.contains("tone=technical"), "{line}");
        assert!(
            line.contains("window_chars=28"),
            "the window tracks the typed text even while inference is off: {line}",
        );
    }

    #[test]
    fn a_live_build_reports_the_classified_mood_and_that_the_model_ran() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.trail_audio = TrailAudio::capturing_for_test();
        type_line(&mut app, wid, "why is this broken again ugh");

        let line = app.tone_status().expect("the fixture window is focused");
        assert!(line.contains("tone=frustrated"), "{line}");
        assert!(line.contains("effective=frustrated"), "{line}");
        assert!(line.contains("audio=live"), "{line}");
        assert!(line.contains("active=true"), "{line}");
        assert!(line.contains("knob=on sounds=on volume=0.40"), "{line}");
        assert!(!line.contains("inferences=0"), "the model ran: {line}");
    }

    #[test]
    fn a_disabled_knob_separates_the_verdict_from_what_the_synth_hears() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.trail_audio = TrailAudio::capturing_for_test();
        type_line(&mut app, wid, "why is this broken again ugh");
        app.config.tone_melody = Some(false);

        let line = app.tone_status().expect("the fixture window is focused");
        assert!(line.contains("tone=frustrated"), "{line}");
        assert!(line.contains("effective=technical"), "{line}");
        assert!(line.contains("knob=off"), "{line}");
        assert!(line.contains("active=false"), "{line}");
    }

    #[test]
    fn a_windowless_app_refuses_rather_than_inventing_a_row() {
        let mut app = App::headless_for_test();
        app.frontmost_window = None;
        assert!(app.tone_status().is_err());
    }

    #[test]
    fn the_status_line_never_carries_the_typed_text() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.trail_audio = TrailAudio::capturing_for_test();
        let secret = "hunter2correcthorse";
        type_line(&mut app, wid, secret);

        let line = app.tone_status().expect("the fixture window is focused");
        assert!(
            !line.contains("hunter") && !line.contains("horse"),
            "the window text must never reach the wire: {line}",
        );
        let chars: Vec<char> = secret.chars().collect();
        for window in chars.windows(4) {
            let needle: String = window.iter().collect();
            assert!(
                !line.contains(&needle),
                "no partial leak either ({needle}): {line}",
            );
        }
        assert!(
            line.contains(&format!("window_chars={}", secret.len())),
            "only the count is reported: {line}",
        );
    }

    fn seam_row_after_one_frame(app: &mut App) -> String {
        let wid = WindowId(0);
        let fx = crate::app_render::CursorFxInputs::sample_for_test(std::time::Instant::now());
        app.cursor_frame(wid, fx).expect("one cursor frame");
        app.tone_status().expect("the fixture window is focused")
    }

    fn audible_app(style: &str) -> App {
        let mut app = App::headless_for_test();
        app.trail_audio = TrailAudio::capturing_for_test();
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some(style.into());
        app.config.trail_sounds = Some(true);
        app
    }

    #[test]
    fn a_dimmed_aurora_reads_trail_off_never_engine_silent() {
        let mut app = audible_app("rainbow kitty");
        app.config.cursor_trail_intensity = Some(0.0);
        let line = seam_row_after_one_frame(&mut app);
        assert!(line.contains("seam=closed:trail-off"), "{line}");
        assert!(line.contains("trail=off"), "{line}");
        assert!(line.contains("engine_sound=closed"), "{line}");
        assert!(!line.contains("engine-silent"), "{line}");
    }

    #[test]
    fn a_reduce_motion_trail_is_dark_but_on_and_heard() {
        let mut app = audible_app("rainbow kitty");
        app.system_reduce_motion = true;
        let line = seam_row_after_one_frame(&mut app);
        assert!(line.contains("seam=open"), "{line}");
        assert!(line.contains("trail=on"), "{line}");
        assert!(line.contains("engine_sound=open"), "{line}");
    }

    #[test]
    fn a_classic_window_is_heard_and_the_verb_says_so() {
        let mut app = audible_app("classic");
        let line = seam_row_after_one_frame(&mut app);
        assert!(line.contains("seam=open"), "{line}");
        assert!(line.contains("engine_sound=open"), "{line}");
        assert!(line.contains("trail=on"), "{line}");
    }
}

/// The inline rename field through the shipping `on_key` with real winit events.
#[cfg(test)]
#[path = "app_input_rename_field_key_tests.rs"]
mod rename_field_key_tests;

/// The person at the keyboard (`human_ms=`, `EVENT <local> human`).
#[cfg(all(test, unix))]
mod person_presence_tests {
    use std::sync::Arc;

    use super::pet_console_input_tests::observing_app as app;
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

    use crate::input::{InputEvent, InputOutcome, PasteFraming, Source};
    use crate::{App, SessionCtx, WindowId};

    const CTL: Source = Source::Controller;

    fn ctx(app: &App) -> Arc<SessionCtx> {
        let session = app.front_terminal(WindowId(0)).unwrap().session;
        app.pool.get(session).unwrap().ctx.clone()
    }

    fn human_events(ctx: &SessionCtx) -> usize {
        ctx.timeline
            .lock()
            .unwrap()
            .since(None)
            .filter(|e| e.kind == "human")
            .count()
    }

    fn typing() -> Vec<InputEvent> {
        vec![
            InputEvent::Key {
                key: Key::Character('x'),
                mods: Modifiers::empty(),
                base_layout: None,
                layout_text: None,
                event_type: KeyEventType::Press,
            },
            InputEvent::Key {
                key: Key::Named(NamedKey::Escape),
                mods: Modifiers::empty(),
                base_layout: None,
                layout_text: None,
                event_type: KeyEventType::Press,
            },
            InputEvent::Text("typed".into()),
            InputEvent::KeySequence(b"\x1b[D".to_vec()),
            InputEvent::Paste("pasted".into(), PasteFraming::AtDrain),
        ]
    }

    fn click(pressed: bool) -> InputEvent {
        InputEvent::MouseButton {
            button: aterm_types::mouse::MouseButton::Left,
            pressed,
            row: 1,
            col: 1,
            mods: 0,
            click_count: 1,
            side: aterm_core::selection::SelectionSide::Left,
            block: false,
            suppress_copy_on_select: false,
            px_off: crate::input::PixelOffset::CELL_ORIGIN,
        }
    }

    fn pointer_move(buttons: u8) -> InputEvent {
        InputEvent::MouseMove {
            buttons,
            row: 2,
            col: 3,
            mods: 0,
            side: aterm_core::selection::SelectionSide::Left,
            px_off: crate::input::PixelOffset::CELL_ORIGIN,
        }
    }

    fn pointing() -> Vec<InputEvent> {
        vec![
            click(true),
            pointer_move(0),
            InputEvent::Wheel {
                dir: aterm_types::mouse::WheelDir::Up,
                lines: 3,
                detent: true,
                row: 1,
                col: 1,
                mods: 0,
                px_off: crate::input::PixelOffset::CELL_ORIGIN,
            },
            InputEvent::ScrollView(crate::input::ScrollIntent::Up),
        ]
    }

    #[test]
    fn a_window_keystroke_stamps_human_ms_and_a_control_write_never_does() {
        for event in typing().into_iter().chain(pointing()) {
            let (mut app, _reader) = app();
            let ctx = ctx(&app);
            assert_eq!(app.input(WindowId(0), event.clone(), CTL), InputOutcome::Ok);
            assert_eq!(ctx.human_input.ms_since(crate::metrics::now_us()), None);
            assert_eq!(
                ctx.human_input.wire(crate::metrics::now_us()),
                "-",
                "{event:?}"
            );
            assert_eq!(human_events(&ctx), 0, "{event:?}");
            assert_eq!(
                app.input(WindowId(0), event.clone(), Source::Human),
                InputOutcome::Ok
            );
            let since = ctx.human_input.ms_since(crate::metrics::now_us());
            assert!(since.is_some_and(|ms| ms < 60_000), "{event:?}: {since:?}");
            assert!(
                ctx.human_input.wire(crate::metrics::now_us()) != "-",
                "{event:?}"
            );
            assert_eq!(human_events(&ctx), 1, "{event:?}");
        }
    }

    #[test]
    fn a_burst_is_one_human_event_and_what_is_not_a_hand_stamps_nothing() {
        let (mut app, _reader) = app();
        let ctx = ctx(&app);
        for event in [
            InputEvent::Key {
                key: Key::Character('x'),
                mods: Modifiers::empty(),
                base_layout: None,
                layout_text: None,
                event_type: KeyEventType::Release,
            },
            pointer_move(3),
            InputEvent::Focus(true),
        ] {
            let _ = app.input(WindowId(0), event, Source::Human);
        }
        assert_eq!(ctx.human_input.ms_since(crate::metrics::now_us()), None);
        for event in typing().into_iter().chain(pointing()).chain([click(false)]) {
            let _ = app.input(WindowId(0), event, Source::Human);
        }
        assert_eq!(human_events(&ctx), 1);
    }
}

#[cfg(test)]
mod native_ime_selection_tests {
    use super::*;
    use crate::native_app::{ActionInvocation, AppEvent, AppViewState, TextInputEvent};
    use crate::native_text_input::TextSelection;

    #[test]
    fn native_ime_preedit_preserves_platform_byte_selection_through_the_input_route() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
        app.dispatch_native_event(
            wid,
            AppEvent::Action(ActionInvocation {
                id: crate::native_ui::ActionId::new("settings/search"),
                value: None,
            }),
        )
        .unwrap();
        app.on_ime_commit(wid, "aβz".to_string());
        let (_, view) = app.active_native_view(wid).unwrap();
        let Some(AppViewState::Settings(state)) = app.native_runtime.view_state_mut(view) else {
            panic!("Settings view");
        };
        state.search_input.set_selection(1, 3);
        let original = state.search_input.clone();

        for (text, cursor, expected) in [
            ("e\u{301}日本", Some((3, 3)), 4..4),
            ("e\u{301}日本", Some((3, 6)), 4..7),
            ("e\u{301}日本", Some((1, 3)), 1..4),
            ("e\u{301}日本", Some((2, 3)), 10..10),
            ("e\u{301}日本", Some((6, 3)), 10..10),
            ("e\u{301}日本", Some((0, 99)), 10..10),
            ("e\u{301}日本", None, 10..10),
            ("👩‍💻日", Some((11, 11)), 12..12),
            ("👩‍💻日", Some((4, 7)), 1..1),
            ("👩‍💻日", Some((5, 7)), 15..15),
        ] {
            app.on_ime_preedit(wid, text.to_string(), cursor);
            let Some(AppViewState::Settings(state)) = app.native_runtime.view_state(view) else {
                panic!("Settings view");
            };
            let projection = state.search_input.projection();
            assert_eq!(state.search_input.value(), "aβz", "preedit is transient");
            assert_eq!(projection.text, format!("a{text}z"));
            assert_eq!(projection.preedit, Some(1..1 + text.len()));
            assert_eq!(
                projection.selection,
                TextSelection {
                    anchor: expected.start,
                    head: expected.end
                },
                "platform selection {cursor:?} in {text:?}"
            );
        }

        let mut lost_range = original;
        lost_range.set_preedit("e\u{301}日本".to_string(), None);
        app.on_ime_preedit(wid, "e\u{301}日本".to_string(), Some((3, 3)));
        let Some(AppViewState::Settings(state)) = app.native_runtime.view_state(view) else {
            panic!("Settings view");
        };
        assert_ne!(
            state.search_input.projection().selection,
            lost_range.projection().selection
        );

        app.on_ime_preedit(wid, String::new(), None);
        let Some(AppViewState::Settings(state)) = app.native_runtime.view_state(view) else {
            panic!("Settings view");
        };
        assert!(state.search_input.preedit().is_none());
        assert_eq!(state.search_input.value(), "aβz");
        assert_eq!(state.search_input.selection().range(), 1..3);
        assert!(app.windows[&wid].preedit.is_empty());

        app.on_ime_preedit(wid, "候補".to_string(), Some((0, 3)));
        app.on_ime_commit(wid, "語".to_string());
        let Some(AppViewState::Settings(state)) = app.native_runtime.view_state(view) else {
            panic!("Settings view");
        };
        assert_eq!(state.search_input.value(), "a語z");
        assert_eq!(state.search_input.selection(), &TextSelection::caret(4));
        assert!(state.search_input.preedit().is_none());
        app.dispatch_native_event(wid, AppEvent::TextInput(TextInputEvent::Undo))
            .unwrap();
        let Some(AppViewState::Settings(state)) = app.native_runtime.view_state(view) else {
            panic!("Settings view");
        };
        assert_eq!(
            state.search_input.value(),
            "aβz",
            "one commit is one undo frame"
        );
        assert_eq!(state.search_input.selection().range(), 1..3);

        use crate::native_app::{NativeApp, SemanticInput};
        use aterm_window::keyboard::{KeyCode, KeyLocation, PhysicalKey, SmolStr};
        let press = |app: &mut App| {
            app.windows.get_mut(&wid).unwrap().mods = ModifiersState::empty();
            for state in [ElementState::Pressed, ElementState::Released] {
                app.on_key(
                    wid,
                    KeyEvent::synthetic_for_test(
                        PhysicalKey::Code(KeyCode::KeyN),
                        Key::Character(SmolStr::new("n")),
                        Some(SmolStr::new("n")),
                        KeyLocation::Standard,
                        state,
                        false,
                    ),
                );
            }
        };
        assert!(app.open_system_state_tab());
        // The worker's read names the plan in force; a page opened by hand
        // submits nothing before it.
        app.publish_system_state(crate::system_state::SetupStatus {
            plan: Some(aterm_types::macos_setup::Plan::default()),
            ..crate::system_state::SetupStatus::default()
        });
        let (instance, _) = app.active_native_view(wid).unwrap();
        for field in ["system-state/url", "system-state/password"] {
            app.dispatch_native_event(
                wid,
                AppEvent::Action(ActionInvocation {
                    id: crate::native_ui::ActionId::new(field),
                    value: Some(SemanticInput::Text(String::new())),
                }),
            )
            .unwrap();
            let projection = |app: &App| {
                let Some(NativeApp::SystemState(model)) = app.native_runtime.app(instance) else {
                    panic!("System State app");
                };
                model.field_projection(field).unwrap()
            };
            app.on_ime_preedit(wid, "候補".into(), Some((0, 3)));
            press(&mut app);
            assert_eq!(
                projection(&app).text,
                if field.ends_with("password") {
                    ""
                } else {
                    "候補"
                },
                "composing physical keys do not insert into {field}"
            );
            if field.ends_with("password") {
                assert!(projection(&app).preedit.is_none());
            }
            app.on_ime_commit(wid, "語".into());
            press(&mut app);
            app.on_ime_preedit(wid, "候補".into(), Some((0, 3)));
            app.on_ime_preedit(wid, String::new(), None);
            press(&mut app);
            assert_eq!(
                projection(&app).text,
                if field.ends_with("password") {
                    "•••"
                } else {
                    "語nn"
                },
                "committed and non-composing input reaches {field} exactly once"
            );
        }
        thread_local! {
            static JOINED: std::cell::RefCell<Vec<(aterm_types::macos_setup::Plan, Option<aterm_secrets::SecretBytes>)>> = const { std::cell::RefCell::new(Vec::new()) };
        }
        app.set_system_state_handlers_for_test(crate::system_state_host::InitialSetupHandlers {
            apply: |plan, _, password, receipt| {
                JOINED.with(|joined| joined.borrow_mut().push((plan, password)));
                receipt.accept();
                true
            },
            ..crate::system_state_host::InitialSetupHandlers::inert_for_test()
        });
        app.dispatch_native_event(
            wid,
            AppEvent::Action(ActionInvocation {
                id: crate::native_ui::ActionId::new("system-state/vault/join"),
                value: None,
            }),
        )
        .unwrap();
        let mut joined = JOINED.with(|joined| std::mem::take(&mut *joined.borrow_mut()));
        assert_eq!(joined.len(), 1, "Join delivers one edited plan");
        let (plan, password) = joined.pop().unwrap();
        assert_eq!(plan.vault_url, "語nn");
        assert_eq!(password.unwrap().as_str(), Some("語nn"));
    }
}

/// The refused-input row's fold: a burst of one cause is one row, and a new cause replaces that
/// row's line under the same key.
#[cfg(test)]
mod refusal_row_tests {
    use super::{InputRefusal, paste_order};

    #[test]
    fn only_a_full_queue_refuses_a_paste_busy() {
        use crate::input::InputOutcome;
        assert_eq!(
            super::rejected_outcome(paste_order::Rejected::Full),
            InputOutcome::QueueFull
        );
        assert_eq!(
            super::rejected_outcome(paste_order::Rejected::TooLarge),
            InputOutcome::WriteFailed
        );
        assert_eq!(
            super::rejected_outcome(paste_order::Rejected::Unavailable),
            InputOutcome::WriteFailed
        );
    }

    #[test]
    fn each_refusal_maps_to_its_cause_and_a_burst_is_one_row() {
        assert_eq!(
            InputRefusal::of_rejected(paste_order::Rejected::TooLarge),
            InputRefusal::PasteTooLarge
        );
        assert_eq!(
            InputRefusal::of_rejected(paste_order::Rejected::Full),
            InputRefusal::Busy
        );
        assert_eq!(
            InputRefusal::of_rejected(paste_order::Rejected::Unavailable),
            InputRefusal::Unavailable
        );
        let mut app = crate::App::headless_for_test();
        app.clear_messages_for_test();
        let key = "session.input-full.7";
        let row = |app: &crate::App| {
            app.messages
                .live_by_key(key)
                .map(|live| (live.id, live.msg.title.clone(), live.msg.detail.clone()))
        };
        app.refuse_input(7, InputRefusal::Unavailable);
        let (first, _, first_detail) = row(&app).expect("the row");
        for _ in 0..5 {
            app.refuse_input(7, InputRefusal::Unavailable);
        }
        assert_eq!(row(&app).map(|r| r.0), Some(first), "a burst is one row");
        app.refuse_input(7, InputRefusal::Busy);
        let (_, _, detail) = row(&app).expect("still one row");
        assert_ne!(detail, first_detail, "a new cause replaces the line");
        assert_eq!(
            app.messages
                .live_rows()
                .filter(|live| live.msg.key.as_deref() == Some(key))
                .count(),
            1
        );
    }
}

#[cfg(any(test, debug_assertions))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "input_release_pairing",
    action = "TransferFocusWhileHeld",
    reason = "OS/winit environment transition: Tier-1 drives real App::on_focus(A,false) then \
              App::on_focus(B,true) and validates that physical ownership is unchanged; no \
              key-owner mutator implements the focus arrival itself."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "input_release_pairing",
    action = "OpenOverlay",
    reason = "Orthogonal overlay environment step; cross-window physical-owner conformance \
              fixes overlays closed, while overlay_gate_pairing_tests drives the real open."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "input_release_pairing",
    action = "CloseOverlay",
    reason = "Orthogonal overlay environment step; cross-window physical-owner conformance \
              fixes overlays closed, while overlay_gate_pairing_tests drives the real close."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "input_release_pairing",
    action = "ConsumeOverlayPress",
    reason = "Controller/engine-key overlay ownership uses overlay_consumed_keys, not the \
              process-wide winit PhysicalKey owner map bound by this Tier-1 runner."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "input_release_pairing",
    action = "RepeatOfConsumedPress",
    reason = "Repeat is a read-only peek of the retained physical owner; dedicated shipping \
              tests prove it neither removes the owner nor emits bytes."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "input_release_pairing",
    action = "GateConsumesRepeatOfForwardedPress",
    reason = "Controller/engine-key repeats enter App::input without a winit PhysicalKey; \
              overlay_gate_pairing_tests drives the real pre-overlay press/open/repeat/release \
              seam. Physical repeats bypass current-content gates through route_physical_repeat."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "input_release_pairing",
    action = "PhysicalFocusLoss",
    reason = "Terminal OS epoch where no matching key-up is delivered. A focus transfer that \
              does deliver key-up is the separately validated TransferFocusWhileHeld action."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "input_release_pairing",
    action = "SettledRelease",
    reason = "Abstract terminal stutter after owner removal; no shipping state mutation exists."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "input_release_pairing",
    action = "SettledFocusEpoch",
    reason = "Abstract terminal stutter after an OS-cancelled epoch; no shipping mutation exists."
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn input_release_pairing_waivers() {}
