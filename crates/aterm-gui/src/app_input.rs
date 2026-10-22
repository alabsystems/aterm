// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keyboard, IME and action dispatch, an inherent-impl split of `App`: the
//! `App::input` convergence seam, `on_key` (keybinding lookup, hardcoded chords,
//! search mode), IME preedit/commit, action and menu dispatch, the seam-left mouse
//! press and `mouse_modifiers`, plus the `egress_to_outcome` reply mapper and the
//! `base_logical_key` cfg pair.

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

/// The input path's one clock read: the stamp every press-like keyboard or paste
/// dispatch shares (`input_now` in [`App::input`]) and the wheel reducer's
/// ([`App::scroll_wheel_animated_with`]). Production reads the wall clock; a test
/// may pin it ([`pin_input_clock`]) so its input and the frames it presents lie
/// on one synthetic timeline: a tracked band starts settling `TRACK_REST_MS`
/// after a delta, and a predicted glyph expires one glitch window after its press,
/// neither of which a stalled test thread may decide.
fn input_clock() -> std::time::Instant {
    #[cfg(test)]
    if let Some(now) = INPUT_CLOCK.with(std::cell::Cell::get) {
        return now;
    }
    std::time::Instant::now()
}

#[cfg(test)]
thread_local! {
    /// The pinned input clock ([`input_clock`]); `None` reads the wall clock.
    static INPUT_CLOCK: std::cell::Cell<Option<std::time::Instant>> =
        const { std::cell::Cell::new(None) };
}

/// Pin (or, with `None`, release) this thread's input clock ([`input_clock`]).
#[cfg(test)]
pub(crate) fn pin_input_clock(now: Option<std::time::Instant>) {
    INPUT_CLOCK.with(|clock| clock.set(now));
}

/// How long after a keystroke the present-pacing bypass (`WindowState::input_hot`)
/// stays armed: longer than a local echo round trip and a fast typist's inter-key
/// gap, short enough that a key which never echoes releases it within a couple of
/// frames. Every key re-arms it, so this is a tail after the last key.
pub(crate) const INPUT_HOT_WINDOW: std::time::Duration = std::time::Duration::from_millis(50);

/// How far ahead of its event-loop arrival a key enqueued on the per-session
/// ordering FIFO stamps the word engine's typed-edit witness: the projected moment it
/// reaches the wire. The witness's echo budget must start at the terminal, and behind
/// a draining paste an arrival stamp would expire in the queue. Safe because the
/// witness is single-use (`TypedEdit::spent`), pane- and caret-reach-scoped, and only
/// for a key that was typed; finite, so a key whose echo never comes stops licensing.
/// Two of the engine's 750 ms echo budgets: licensed at most ~2.25 s after the press.
const ORDERED_EGRESS_WITNESS_GRACE: std::time::Duration = std::time::Duration::from_millis(1_500);

/// Per-session evidence that recently accepted input can explain output.
///
/// The tracker lives in [`crate::SessionCtx`], not a window: one session may be
/// visible in several windows or panes, and a delayed paste may complete after
/// focus moved. Renderers take only non-parking lock attempts, so attribution
/// adds neither a registry lock, a grid scan, nor a UI-thread wait.
#[derive(Default)]
pub(crate) struct OutputEchoTracker {
    active_writes: AtomicUsize,
    /// Every attempt to write input to the session (a key, a paste, a raw editor
    /// frame, a controller's `send`), counted at [`Self::begin`]. Reports (focus,
    /// mouse, colour scheme) and engine replies (DA, DSR) never pass here, so this
    /// counts what a person or controller sent ([`Self::input_sent`]).
    sent: AtomicU64,
    published: Mutex<OutputEchoPublished>,
}

/// How a paste's insert is priced for the cursor engine ([`DeliveryTicket::insert`]),
/// from the text the formatter sends: a single-line body in cells
/// (`Terminal::paste_insert_cells`); one line of more than zero cells and the one
/// line break after it (`LF`, `CR` or `CR LF`, which go out as one `CR`: a link
/// copied with its line ending) as [`InsertWidth::Line`], so the line is lit when
/// its echo draws the break's new line in the same frame; any other body is the
/// unknown class.
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

/// What a queued write's completion re-stamps on the cursor engines. A paste, and
/// every key typed while one drains, is written on the `aterm-egress-order` thread;
/// enqueue is not delivery, so the licences revoked at enqueue return at the
/// completed write. The ticket rides the FIFO job, is published as a
/// [`DeliveryReceipt`] when the sink accepts the bytes (a failed one when the write
/// fails, [`revoke_failed_delivery`]), and the render prelude applies it to the
/// window's engines (`App::cursor_frame`). Inline writes carry none: their arrival
/// stamp is the licence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DeliveryTicket {
    /// A delivered insert's width: a paste priced from its text
    /// (`Terminal::paste_insert_cells`), or the unknown class (a Tab, a ⌃V, an
    /// unpriceable body) (`CursorGlow::note_insert_delivered`). A type, not a
    /// sentinel width, since the two classes are spent under different row laws.
    pub(crate) insert: Option<InsertWidth>,
    /// The window that dispatched a key: the one whose engines banked the press, and
    /// the only one the key class is re-stamped on. Another window on the same
    /// session banked nothing, so it re-banks nothing. `None` for a paste, whose
    /// insert half applies on whichever window reads the session's receipt.
    /// scope-waiver: "session-wide" here names the insert half's fan-out, not a
    /// safety budget; it only arms the paste's own insert licence on each engine
    /// that reads it.
    pub(crate) window: Option<WindowId>,
    /// The key's licence class, revoked at enqueue and re-stamped at delivery on
    /// [`Self::window`] (`CursorGlow::note_delivered`), so a key queued behind a paste
    /// is licensed exactly like an inline key.
    pub(crate) key: Option<DeliveredClass>,
    /// When the bytes were dispatched at the input seam, carried to the delivery
    /// edge so the insert's late-receipt claim
    /// (`CursorGlow::note_insert_delivered_from`) reaches back no further than the
    /// press; a Tab queued behind a paste could otherwise claim a program hop
    /// refused before its own dispatch.
    pub(crate) dispatched_at: std::time::Instant,
}

impl DeliveryTicket {
    fn is_empty(self) -> bool {
        self.insert.is_none() && self.key.is_none()
    }
}

/// One completed delivery: the ticket, its serial (monotone per session),
/// and when the sink accepted the bytes — `0` while they still sit in the
/// process-local spill (a wedged foreground), resolved to the drain instant
/// by the reader exactly as `sample()` defers `last_accepted_us`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DeliveryReceipt {
    pub(crate) serial: u64,
    delivered_us: u64,
    pub(crate) ticket: DeliveryTicket,
    /// The write FAILED: nothing reached the PTY and nothing ever will, so the
    /// reader takes back what the dispatch banked instead of re-stamping it
    /// ([`Deliveries::failed`]).
    failed: bool,
}

/// How many delivery receipts the tracker keeps for the render prelude: a paste, every
/// key typed behind it (up to the press bank's depth) and other ticketed work can drain
/// before one frame reads them, so two press-bank depths. Beyond this,
/// [`Deliveries::evicted`] reports the lost receipts.
pub(crate) const DELIVERY_RING: usize = 2 * aterm_effects::cursor_glow::TYPED_STAMP_DEPTH;

/// The deliveries a window has not yet applied, in serial order
/// ([`OutputEchoTracker::deliveries_after`]).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Deliveries {
    /// The newest resolved serial — what the window records as seen.
    pub(crate) latest: u64,
    /// `(delivery instant on the frame's clock, ticket)`, oldest first.
    pub(crate) items: [Option<(std::time::Instant, DeliveryTicket)>; DELIVERY_RING],
    /// The QUEUED writes that FAILED, oldest first: the render prelude takes
    /// back what each one's dispatch banked ([`revoke_failed_delivery`]).
    pub(crate) failed: [Option<DeliveryTicket>; DELIVERY_RING],
    /// Receipts newer than the reader's baseline that the ring had overwritten
    /// before this read, reported once: on the read whose `latest` advances past
    /// them (a read held at its baseline by a spill-held receipt reports zero). The
    /// licences they carried cannot be reconstructed.
    pub(crate) evicted: u64,
}

impl Deliveries {
    /// Nothing to apply, read up to `latest`.
    fn none_after(latest: u64) -> Self {
        Self {
            latest,
            items: [None; DELIVERY_RING],
            failed: [None; DELIVERY_RING],
            evicted: 0,
        }
    }
}

/// Acceptance-ordered echo evidence. The sink token, timestamps, and spill mode
/// move under one tiny lock so a renderer can never combine different writes.
struct OutputEchoPublished {
    order: Option<AcceptedOrder>,
    last_accepted_us: u64,
    last_boundary_us: u64,
    /// `Some(true)` refreshes the accepted-input shadow after a spilled echoable
    /// frame drains; `Some(false)` merely keeps a boundary hot until it drains.
    spill_debt: Option<bool>,
    /// The delivery register: the last [`DELIVERY_RING`] completed ticketed writes,
    /// a ring the render prelude drains by serial. A ring because a multi-file drop
    /// is one paste per file and several can complete in one frame.
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
    fn push_receipt(&mut self, receipt: DeliveryReceipt) {
        let head = self.delivery_head;
        self.deliveries[head] = Some(receipt);
        self.delivery_head = (head + 1) % DELIVERY_RING;
    }

    /// A receipt held in the spill settles at the first instant the spill is seen
    /// empty; its bytes reached the kernel no later than that. Settled at the publish
    /// as well as the read so instants stay monotone in serial order and a spill that
    /// restarts cannot re-hold receipts already on the wire.
    fn settle_held_receipts(&mut self, stamp_us: u64) {
        for r in self.deliveries.iter_mut().flatten() {
            if r.delivered_us == 0 {
                r.delivered_us = stamp_us;
            }
        }
    }
}

/// A metrics-clock stamp (`crate::metrics::now_us`) placed on the frame's clock: the
/// instant it was read at ([`crate::metrics::instant_at_us`]), never later than `now`.
/// Exact, not reconstructed from a second clock read, which could invert two close
/// stamps from different frames (a paste receipt and the Enter behind it); the
/// verdict's spend latch (`verdict_armed`) identifies an Enter by this instant. The
/// metrics clock is one fixed `Instant` anchor, so placement is a pure function of
/// the stamp, ordered across samples; `now` only bounds a write accepted after the
/// frame's clock was read.
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
    /// The facts PRISM WAKE's licence reads.
    pub(crate) fn streak(self) -> aterm_effects::pipeline::StreakEcho {
        aterm_effects::pipeline::StreakEcho {
            boundary_at: self.last_boundary_at,
            accepted_at: self.last_accepted_at,
            input_hot: self.input_hot,
        }
    }
}

/// RAII ownership of one real egress attempt.  Dropping on an early return or
/// panic cannot strand a session permanently hot.
pub(crate) struct OutputEchoWrite<'a> {
    tracker: &'a OutputEchoTracker,
    counted: bool,
    kind: OutputEchoInput,
    /// The queued write's [`DeliveryTicket`], published on acceptance.
    ticket: Option<DeliveryTicket>,
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
        if kind != OutputEchoInput::Ignored {
            self.sent.fetch_add(1, Ordering::AcqRel);
        }
        let counted = kind != OutputEchoInput::Ignored
            && self
                .active_writes
                .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                    active.checked_add(1)
                })
                .is_ok();
        // On the physically unreachable overflow case the saturated non-zero
        // count is already fail-closed (hot), and this guard must not decrement
        // a write it did not count.
        OutputEchoWrite {
            tracker: self,
            counted,
            kind,
            ticket: None,
        }
    }

    /// [`Self::begin`] for a queued write carrying a [`DeliveryTicket`]: the writer
    /// thread's completion publishes the ticket as a receipt.
    fn begin_ticketed(
        &self,
        kind: OutputEchoInput,
        ticket: Option<DeliveryTicket>,
    ) -> OutputEchoWrite<'_> {
        let mut write = self.begin(kind);
        write.ticket = ticket.filter(|t| !t.is_empty());
        write
    }

    /// How many input writes have been attempted on the session: keys,
    /// pastes, raw editor frames and controller sends — never a report nor an
    /// engine reply. A change between two reads means someone typed or sent
    /// something in between (`handoff_history::InputWatch`).
    #[must_use]
    pub(crate) fn input_sent(&self) -> u64 {
        self.sent.load(Ordering::Acquire)
    }

    pub(crate) fn begin_event(&self, ev: &InputEvent) -> OutputEchoWrite<'_> {
        self.begin(OutputEchoInput::of(ev))
    }

    /// The render prelude's read: every delivery newer than `after`, oldest first,
    /// placed on the frame's clock ([`stamp_on_frame_clock`]); a receipt still in the
    /// spill holds `latest` back until the sink reports it drained. A blocking lock,
    /// unlike `sample()`: the writer holds it only for a few field writes, and the
    /// frame that observes an insert's echo must not miss its receipt. `None` (a
    /// fresh window, a session switch) gets the newest serial and nothing to apply.
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
        // Read oldest first straight off the ring: `publish` writes at
        // `delivery_head` with a monotone serial. The apply stops at the first
        // spill-held receipt (`delivered_us == 0`), which holds `latest` back.
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
            // A receipt is never before its dispatch: the cursor-glow hop claim
            // (`read_deliveries`) decides whether a hop refused before the key was
            // dispatched may be lit as its completion, so the `now`-bounded
            // placement is clamped to `dispatched_at`.
            let at = stamp_on_frame_clock(r.delivered_us, now).max(r.ticket.dispatched_at);
            *slot = Some((at, r.ticket));
            out.latest = r.serial;
        }
        // Serials past the baseline the ring no longer holds were evicted unread,
        // counted per reader and reported once, on the read that advances past them.
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

    /// Samples echo evidence against the render frame's injected clock. Each stamp is
    /// placed at the instant it was read, bounded by `now` ([`stamp_on_frame_clock`]),
    /// so it keeps its place and order whichever sample hands it over.
    pub(crate) fn sample(&self, sink: &SinkWriter, now: std::time::Instant) -> OutputEchoSample {
        let clock_us = crate::metrics::now_us().max(1);
        let mut published = match self.published.try_lock() {
            Ok(published) => published,
            Err(std::sync::TryLockError::Poisoned(poison)) => poison.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                // A writer is publishing the one coherent record. Never put the
                // event loop behind it: one conservative hot frame is cheaper
                // than either a mixed snapshot or a UI-thread wait.
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

    /// Publish one state-affecting acceptance in sink order, and its delivery
    /// ticket if any, in one `published` hold, so no read sees the acceptance
    /// without its receipt. A writer may be descheduled after the sink accepts its
    /// bytes and before it reaches this method; the opaque sink token makes a late
    /// older completion a no-op. A ticket's spill probe parks (a contended mutex is
    /// not bytes in the spill; spill holds are microseconds) and runs before
    /// `published` is taken, so the prelude's read never waits behind it.
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
                    // Bare Enter submits a turn. Its response is authored output,
                    // not editor echo, so it clears the direct shadow. The false
                    // debt stays hot only while accepted bytes remain spilled.
                    published.last_boundary_us = stamp;
                    published.last_accepted_us = 0;
                    published.spill_debt = Some(false);
                }
                OutputEchoInput::Ignored => unreachable!("ignored input has no echo publication"),
            }
            // A frame that went whole to the kernel on the direct lane needs no
            // spill-mutex probe: the sink's receipt says it drained. Any other frame
            // takes its ticket's probe if that found the spill drained, else asks
            // (non-parking `try_lock`).
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

    /// Publish one ticketed write that FAILED: the writer thread's answer for a
    /// queued key whose bytes never reached the PTY. Its enqueue kept the press
    /// credit, since a queued key's bytes WILL reach the wire
    /// ([`revoke_dispatch`]); these never will, and the credit would fund a later
    /// cursor move of the program's own as the key's echo. On Windows every key is
    /// queued (the ConPTY writer), so a pseudoconsole that has gone left every key
    /// typed into it banked; on unix a key queued behind a draining paste whose
    /// session dies does the same. Stamped at once: nothing of it waits in the
    /// spill.
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
    /// Complete with the seam's narrow receipt.  Zero-byte success and failed
    /// writes never manufacture a post-return echo timestamp; an explicitly
    /// accepted guarded prefix does.
    fn finish(mut self, receipt: input::EgressReceipt, sink: &SinkWriter) {
        if self.kind != OutputEchoInput::Ignored
            && let Some(order) = receipt.accepted_order()
        {
            let stamp = crate::metrics::now_us().max(1);
            // The delivery edge rides the same `published` lock hold as the
            // acceptance publication: a zero-byte or failed write publishes
            // no delivery, so a swallowed queued key stays revoked.
            let ticket = self.ticket.take();
            self.tracker
                .publish(order, self.kind, stamp, receipt.is_direct(), ticket, sink);
        } else if receipt.egress == input::Egress::Reported(input::Delivery::Failed)
            && let Some(ticket) = self.ticket.take()
        {
            // THE FAILED EDGE: nothing landed and nothing will, so the reader
            // takes back what the dispatch banked ([`OutputEchoTracker::publish_failed`]).
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

/// One receipt-bearing, echo-aware direct egress. Control-thread targets use
/// this when they cannot post `Wake::Input`; the source-blind encoder remains
/// the sole byte authority.
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

/// The one egress law: begin the echo write (kind derived from the event, ticket
/// published as a [`DeliveryReceipt`] on acceptance), write through the seam, and
/// finish with the receipt. Every tracked write (inline, the FIFO writer, a
/// control-thread target) goes through here, so tickets and kinds each have one
/// source.
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

/// [`tracked_egress_ticketed`] for a large paste carrying its
/// [`aterm_session::sink::BulkMeter`]: the same egress law, the paste's frame
/// written through the metered sink write.
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
    let write = tracker.begin_ticketed(OutputEchoInput::of(ev), ticket);
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

/// The IME composition clock's transition on one preedit event
/// (`WindowState::ime_compose`): a non-empty preedit starts the clock unless one is
/// open, or restarts it (counting the old one cancelled) after a cleared,
/// uncommitted composition; an empty preedit marks the open one cleared, keeping its
/// start for the commit that usually follows.
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

/// The surface that owns one window's in-flight IME composition, resolved per frame
/// by [`App::preedit_owner`] through the routing [`App::on_ime_commit`] dispatches
/// on, so where the composition paints and where its commit lands agree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PreeditOwner {
    /// No composition in flight — every splice is a zero-cost no-op.
    None,
    /// A frontmost native view (Settings, editor): its own text-input pipeline paints
    /// the preedit (`App::native_preedit_active`); the grid splices stay out.
    Native,
    /// The inline tab-rename field: the composition belongs in its well (the more
    /// deeply focused field; its key gate sits above find's).
    Rename,
    /// The open find field: the composition belongs in the find bar's well.
    Find,
    /// No field has focus: the composition splices into the grid at the caret.
    Grid,
}

/// A predictor mutation needs a paint when it removes pixels already on glass or
/// creates a visible overlay, never keyed on the (usually `false`) "new prediction
/// added" return of a flush path.
#[inline]
const fn prediction_visibility_requires_redraw(was_visible: bool, is_visible: bool) -> bool {
    was_visible || is_visible
}

// Its truth table, checked at compile time: a flush removing a visible ghost
// repaints, a new overlay repaints, and hidden bookkeeping on both sides requests
// no frame.
const _: () = assert!(
    prediction_visibility_requires_redraw(true, false)
        && prediction_visibility_requires_redraw(false, true)
        && !prediction_visibility_requires_redraw(false, false)
);

/// Everything the key-time typing click needs from App-level state to reach the
/// synth without a render tick, resolved before the window borrow and only when
/// [`keystroke_click_audible`] said a click could sound. `tone_melody` is the knob,
/// not the verdict: the verdict lives on the window's `tone_tracker` and resolves
/// through the same `ToneTracker::effective` the drain seams use.
#[derive(Clone, Copy)]
struct KeyClickSynth {
    style: crate::cursor_glow::GlowStyle,
    voice: aterm_effects::trail_sound::SoundVoice,
    gain: f32,
    bed: bool,
    tone_melody: bool,
}

/// Whether a typing click cued at the key could reach a speaker: the host half of
/// the touch-to-glass audio seam (`CursorGlow::cue_keystroke`). The engine owns the
/// darkness law (master switch, geometry, the host's `GlowConfig::audible` focus
/// verdict; not the folded amplitude, whose accessibility stage and load-shed
/// envelope may not mute typing); this answers whether a click cued now could sound,
/// so a build whose sound can only be silence (headless, a non-macOS stub, a failed
/// worker, the knob off, volume 0) never pays the cue-delivering redraw on its
/// hottest path.
///
/// It must agree with the drain: cueing at the key arms a credit the echo's `spawn`
/// spends to stay silent (one character, one click), so a cue the drain dropped for
/// a policy this gate ignored would mute the echo for a click that never sounded.
/// The drain's two mute policies, serious mode (`SeriousEffect::TerminalSound`) and
/// the post-resize quiet window, are checked here too. Focus is not a term: a
/// physical keypress arrives at the focused window. A policy that flips between key
/// and drain orphans a credit only until the ledger's freshness window expires it.
#[inline]
pub(crate) fn keystroke_click_audible(
    host: crate::trail_audio::HostState,
    sounds_on: bool,
    volume: f32,
    sound_allowed: bool,
    resize_quiet: bool,
) -> bool {
    // `sound_seam` with the terms the key path must not re-judge pinned open.
    crate::sound_seam::sound_seam(&crate::sound_seam::SeamInputs {
        sounds_on,
        volume,
        serious_allows: sound_allowed,
        resize_quiet,
        // Pinned open: the key path keeps pushing under a wedged host, since the
        // push drives `revive_or_seal`, and the trail, focus and engine terms are
        // the engine's own gate (`CursorGlow::cue_keystroke_shifted`). Only
        // `HostState::Inert`, the complement of `TrailAudio::is_live()`, closes it.
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

/// What kind of glyph the key landed, for Rainbow Kitty's engine
/// ([`aterm_effects::rainbow_kitty::TypedClass`]): the spacebar's rest, the `!`
/// hero, a capital (asked of the glyph, however it was shifted), and every other
/// printable. A committed IME run (`typed == None`) is `Glyph`. Pure.
#[inline]
pub(crate) fn typed_class_for(typed: Option<char>) -> aterm_effects::rainbow_kitty::TypedClass {
    use aterm_effects::rainbow_kitty::TypedClass;
    match typed {
        Some(' ') => TypedClass::Space,
        Some('!') => TypedClass::Bang,
        Some(c) if c.is_uppercase() => TypedClass::Capital,
        _ => TypedClass::Glyph,
    }
}

/// The synth's glyph class ([`aterm_effects::trail_sound::EventMeta::glyph_class`]):
/// the sixteen-way keyboard table ([`aterm_effects::trail_sound::glyph_class`]:
/// letter, `?`, `!`, digit, stop, open, close, quote, line, rise, math, sigil,
/// comma, semicolon, colon, dash). Filled at the keyed seam only; an echo-born cue
/// has no key and carries `0`.
///
/// The table lives with the field it fills
/// ([`aterm_effects::trail_sound::typed_glyph_class`]) so its consumers (this seam
/// and the engine's tests) share one copy.
#[inline]
pub(crate) fn typed_glyph_class(typed: Option<char>) -> u8 {
    aterm_effects::trail_sound::typed_glyph_class(typed)
}

/// What the keyed click says about its key: the gesture kind, the shift, the glyph
/// class and the alphabet rank.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct KeyClickStamp {
    pub(crate) kind: aterm_effects::trail_sound::SoundKind,
    pub(crate) shifted: bool,
    pub(crate) glyph_class: u8,
    pub(crate) rank: u8,
}

/// The keyed click's stamp: normally the key's own (spacebar or typed click, the
/// capital's shift, the glyph's class and rank). When the tty is in canonical
/// no-echo mode (`secret`: `sudo`, an `ssh` passphrase, `read -s`) every key gets one
/// stamp (`Typed`, unshifted, class
/// [`aterm_effects::trail_sound::glyph_class::SECRET`], rank 0), so the click says a
/// key was pressed and nothing about which.
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

/// The keyed seam's `at_ms`: the hardware arrival ([`crate::metrics::key_arrival_ms`])
/// when the press armed one, else the host input clock. Read before the PTY write,
/// which consumes the arrival, so the write's duration stays out of the melody.
#[inline]
fn key_sound_at_ms() -> u32 {
    crate::metrics::key_arrival_ms().unwrap_or_else(crate::app_render::input_clock_ms)
}

/// The synth's glyph rank ([`aterm_effects::trail_sound::EventMeta::rank`]): the
/// lossy alphabet position the derived melody reads, filled at the keyed seam only
/// (the engine's [`aterm_effects::trail_sound::typed_glyph_rank`]).
#[inline]
pub(crate) fn typed_glyph_rank(typed: Option<char>) -> u8 {
    aterm_effects::trail_sound::typed_glyph_rank(typed)
}

/// Whether an input event carries fresh, discrete intent that may start one new
/// bounded presentation-recovery episode. Pointer motion does not: a hover or drag
/// stream is unbounded, and treating it as fuel would turn a finite retry train
/// into an endless render loop while a surface is unavailable.
#[inline]
pub(crate) fn is_present_recovery_stimulus(ev: &InputEvent) -> bool {
    !matches!(
        ev,
        InputEvent::Key {
            event_type: aterm_types::keyboard::KeyEventType::Release,
            ..
        } | InputEvent::MouseMove { .. }
    )
}

/// Test-only observation of the real physical-release routing branch, compiled out
/// of shipping builds. Tier-1 uses it to prove a cross-window key-up was swallowed
/// for a consumed press or encoded against the exact press-time
/// session/key/modifier identity for a forwarded press.
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

/// Compatibility `open prefs|about|update` spellings resolve to durable routes in
/// the process-singleton Settings tab.
fn native_settings_route_for_aux(
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
        | AuxTarget::Overview => None,
    }
}

/// Configurable actions that are meaningful without terminal authority. Raw
/// key sequences are never native-safe; terminal-only actions are consumed as
/// no-ops while a native view owns input.
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

// The native-view capability boundary, checked at compile time rather than by
// a test that could be skipped.
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
    // Choosing a theme owes the terminal nothing.
    assert!(native_binding_allowed(Action::ChooseTheme));
    // F11 is a window command and keeps working over a Settings or markdown tab.
    assert!(native_binding_allowed(Action::ToggleFullscreen));
    assert!(!native_binding_allowed(Action::FontIncrease));
    assert!(!native_binding_allowed(Action::FontReset));
    assert!(!native_binding_allowed(Action::JumpPrevPrompt));
    assert!(!native_binding_allowed(Action::JumpNextPrompt));
    // Select All is capability-routed separately to the native view's text
    // model, never the parked terminal's selection.
    assert!(!native_binding_allowed(Action::SelectAll));
    // Copy is capability-routed separately so it can never consult the parked
    // terminal selection beneath a native tab.
    assert!(!native_binding_allowed(Action::Copy));
};

/// Per-session ordered egress serializer, so pastes and keystrokes reach the PTY in
/// submission order.
///
/// A paste's blocking write runs off the UI thread so a large paste into a stalled
/// child cannot freeze the event loop, and any key submitted while one is in flight
/// goes through the same per-session FIFO, drained by a single writer thread running
/// the same [`crate::input::seam_egress_receipt`] as the inline path. The key hot
/// path reads only its own sink's pending-job atomic, never a process-wide lock:
/// `App::input` runs on the UI thread, so a session's count is raised before the
/// next key chooses inline delivery and lowered only after the preceding write.
///
/// Bounded: one budget per serializer (`Budget`, `Limits`: jobs and retained
/// capacity, including the job being written), and pastes and UI-encoded reports
/// may never spend the keys' reserve. A job the budget cannot hold, or a gone
/// writer, is refused at submission: nothing queued or written, no bypass, no wait
/// on the UI thread ([`crate::input::EgressReceipt::queue_full`]). Teardown is the
/// sink's sever (`Session::drop` → [`SinkWriter::sever_input`]): the parked job
/// abandons its frame and every job behind it fails (on Windows a write inside
/// ConPTY's `WriteFile` ends only when that call returns). On Windows this writer is
/// also the ConPTY writer for every key and report ([`ordered_or_inline`],
/// [`report_ordered_or_inline`]).
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

    /// Bounds include the job currently writing. Paste cannot spend the last
    /// ordinary-input allowance, so a full paste queue still accepts keys in
    /// their original order. The byte charge includes owned capacity, not len.
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
        /// High 32 bits: jobs; low 32 bits: retained bytes. One CAS observes
        /// and reserves both dimensions while the worker releases concurrently.
        used: AtomicU64,
        limits: Limits,
    }

    impl Budget {
        fn new(limits: Limits) -> Self {
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

        /// Whether a charge of `bytes` fits this queue at all, however empty:
        /// the byte bound [`Self::claim`] tests, with nothing retained.
        fn could_ever_hold(&self, bytes: u32, paste: bool) -> bool {
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

    /// Owns the admission until all of a job's data is dropped. Completion,
    /// channel failure and unwinding release the same claims exactly once.
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
        /// The queue is full now; it frees as earlier input is written.
        Full,
        /// The input's charge (owned capacity included) exceeds the queue even
        /// when it is empty — waiting cannot help, splitting it can.
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

    /// A deferred egress: run `seam_egress(term, sink, ev)` on the writer thread.
    struct Job {
        term: Arc<Mutex<Terminal>>,
        modes: Arc<ModeMirror>,
        sink: Arc<SinkWriter>,
        /// The session's echo tracker ([`tracked_egress_metered`]); `None` for a report
        /// the UI thread already encoded, whose verbatim `KeySequence` is not typing.
        echo: Option<Arc<OutputEchoTracker>>,
        ev: InputEvent,
        /// What this write's completion re-stamps on the cursor engines; `None` for
        /// a write no licence waits on.
        ticket: Option<DeliveryTicket>,
        /// The key-arrival stamp claimed at enqueue, so the writer books the true
        /// key→write slice; `0` with no keystroke behind the job.
        key_ns: u64,
        /// A visible press queued behind this sink's paste, never the paste or
        /// a key release. Kernel delivery renews the short echo-present window.
        hot_key: Option<(u64, Option<EventLoopProxy<Wake>>)>,
        /// A large paste's progress and stop: its frame is written through the
        /// metered sink write, and a stop pressed while it waits here writes nothing.
        meter: Option<Arc<BulkMeter>>,
        completion: Option<crate::control_input_ack::InputCompletion>,
        /// Who waits for this write's own receipt ([`Written`]): a control lane's
        /// reply. Dropped unsent with a job that never runs, which the waiter reads
        /// as nothing written.
        written: Option<SyncSender<crate::input::EgressReceipt>>,
        /// Last field: payloads above are dropped before capacity is returned.
        _reservation: Reservation,
    }

    /// The receipt of a write handed to the session's ordered writer, for a control
    /// lane that answers from the write rather than from its admission
    /// ([`unparked_observed`]).
    pub(crate) struct Written(Receiver<crate::input::EgressReceipt>);

    impl Written {
        /// The write's receipt once the writer has run it, waiting at most `bound`:
        /// `Err(Timeout)` while it is still queued or writing, `Err(Disconnected)`
        /// when it was dropped unrun (stopped, or its session severed): nothing of
        /// it was written.
        pub(crate) fn wait(
            &self,
            bound: Duration,
        ) -> Result<crate::input::EgressReceipt, RecvTimeoutError> {
            self.0.recv_timeout(bound)
        }

        /// Test-only: a receipt and the sender the writer would answer it on.
        #[cfg(test)]
        #[cfg_attr(not(unix), allow(dead_code))] // its tests are unix-only
        pub(crate) fn for_test() -> (SyncSender<crate::input::EgressReceipt>, Written) {
            let (tx, rx) = sync_channel(1);
            (tx, Written(rx))
        }
    }

    /// ConPTY has no non-blocking write, so an inline key or report on Windows could
    /// park the UI thread on a program that stopped reading. There the per-session
    /// ordered writer is the ConPTY writer, and every key ([`ordered_or_inline`]) and
    /// report ([`report_ordered_or_inline`]) goes through it. On unix the inline write
    /// cannot park ([`SinkWriter::write_frame_interactive_with_receipt`]), so only
    /// input behind a queued paste takes the FIFO.
    const UI_WRITES_CAN_BLOCK: bool = cfg!(windows);

    #[cfg(test)]
    thread_local! {
        /// Test-only: this thread's App paths take the Windows route
        /// ([`force_conpty_route_for_test`]).
        static CONPTY_ROUTE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// The platform's routing fact ([`UI_WRITES_CAN_BLOCK`]); a test may force the
    /// Windows route for its own thread.
    fn ui_writes_can_block() -> bool {
        #[cfg(test)]
        if CONPTY_ROUTE.with(std::cell::Cell::get) {
            return true;
        }
        UI_WRITES_CAN_BLOCK
    }

    /// Test-only: routes this thread's keys and reports as on Windows until the guard
    /// drops.
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

    /// Test-only: the routes a test on this host can drive, `false` the inline one
    /// and `true` the ConPTY one ([`force_conpty_route_for_test`]). Windows has the
    /// ConPTY route alone; a unix run drives both, so the tests that run there keep
    /// the Windows route honest too.
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
        /// Test-only: jobs this thread handed to an ordered writer
        /// ([`submitted_on_this_thread_for_test`]).
        static SUBMITTED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }

    /// Test-only: how many jobs this thread has handed to an ordered writer. On the
    /// ConPTY route an event that reaches the seam is FIFO admission (`Ok`, with the
    /// write, and a stub sink's failure, later on the writer thread), so this count
    /// tells it from an event the App swallowed, which also answers `Ok`.
    #[cfg(test)]
    pub(crate) fn submitted_on_this_thread_for_test() -> u64 {
        SUBMITTED.with(std::cell::Cell::get)
    }

    /// One session's serializer: the FIFO sender and a `Weak` to the session
    /// sink so a closed tab's entry can be pruned. The pending-job count lives
    /// on that sink, which the hot path already holds.
    struct Serializer {
        tx: SyncSender<Job>,
        sink: Weak<SinkWriter>,
        budget: Arc<Budget>,
    }

    /// session master key -> serializer. Created lazily on first use, pruned once
    /// the session's sink is gone.
    static REG: LazyLock<Mutex<HashMap<i32, Serializer>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    /// A queued key is the only completed write that can renew echo-present
    /// priority. Only a full, non-empty direct receipt qualifies: a spill receipt is
    /// FIFO admission, and waiting for its whole-sink drain would hold the session's
    /// sole ordered writer behind unrelated later input.
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

    /// The writer thread body: drain jobs in FIFO order, writing each through the
    /// seam the inline path uses. The registry retains its sender until a later
    /// submission prunes it, so an idle writer also retires after its sink dies. A
    /// queued job owns the sink, and a live sink always keeps this worker.
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
        // This writer thread is expendable: block under SPILL_CAP so a wedged
        // foreground applies backpressure here rather than growing the spill. The
        // finish publishes the delivery ticket at sink acceptance, and the metric
        // books hardware key arrival → completed write, including queue time.
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
            // An encoded report: its bytes, verbatim, untracked like the
            // inline report it stands in for.
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
        // Echo round trip for a key that waited here: the UI thread's bracket
        // closed before this write, so the clock arms at the write's acceptance,
        // carried by the receipt's accepted order. Visible presses only
        // (`hot_key`): a paste never arms, and a swallowed password key would only
        // expire.
        if let Some((session, _)) = job.hot_key
            && receipt.fully_accepted_nonempty()
        {
            crate::echo_rtt::arm_deferred(session, &job.sink);
        }
        if let Some((session, proxy)) = job.hot_key.take()
            && let Some(wake) = completed_queued_key_wake(receipt, Some(session))
            && let Some(proxy) = proxy
        {
            // A reader can post the echo wake before this writer returns.
            // Reconsider a content frame already deferred at the full,
            // pre-key interval. A spill merely retires its FIFO job and
            // preserves the original bounded arrival-time priority.
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
            // A fresh actor/factory could mark: dropping an unrun job must not.
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
            // None of those outcomes consumed this factory's first-acceptance gate.
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

    /// The FIFO sender for `master`, spawning the writer thread
    /// on first use. Failure rejects input; bypassing this serializer could
    /// overtake accepted work or block the event loop.
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
        if let Some(s) = reg.get(&master) {
            return Some((s.tx.clone(), Arc::clone(&s.budget)));
        }
        let (tx, rx) = sync_channel::<Job>(limits.jobs as usize);
        let budget = Arc::new(Budget::new(limits));
        let weak_sink = Arc::downgrade(sink);
        crate::qos::spawn_lane_thread(
            "aterm-egress-order",
            crate::qos::Role::Interactive,
            move || {
                // macOS: this thread does the real PTY write for keys the UI thread
                // handed off (`enqueue` transfers the key-arrival stamp), so it takes
                // the UI thread's urgency rather than default QoS, or a key queued
                // behind a paste would wait on an E-core-parked thread.
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

    /// Whether this sink's egress must be ordered behind a submitted paste or
    /// key. One acquire load, independent of every other session's queue.
    pub(crate) fn is_ordering(sink: &SinkWriter) -> bool {
        sink.ordered_egress_count() > 0
    }

    /// Wait until this sink's ordered egress has drained: a hang detector
    /// ([`aterm_spec::wait::MUST_HAPPEN`]); the caller asserts the drain.
    #[cfg(test)]
    pub(crate) fn await_drain_for_test(sink: &SinkWriter) {
        let _ = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            std::time::Duration::from_millis(1),
            || (!is_ordering(sink)).then_some(()),
        );
    }

    /// The update handoff's cold drain gate, by master: its live-master list can
    /// outlast a session's pool entry while a queued job keeps the sink alive.
    #[cfg(unix)]
    pub(crate) fn is_master_ordering_for_handoff(master: i32) -> bool {
        REG.lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&master)
            .and_then(|serializer| serializer.sink.upgrade())
            .is_some_and(|sink| sink.ordered_egress_count() > 0)
    }

    /// Test seam: keep the cold writer registry locked while another session
    /// makes its hot-path ordering decision. The returned guard is deliberately
    /// opaque so tests cannot inspect or mutate the registry's private state.
    #[cfg(all(test, unix))]
    pub(crate) fn hold_registry_for_test() -> impl Drop {
        REG.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Enqueue `ev` onto `master`'s FIFO so it reaches the PTY in submission order.
    /// A rejection accepts no bytes and must never fall back around the FIFO.
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

    /// The one submission body behind [`enqueue_metered`] and the report
    /// route: `echo` is `None` only for an encoded report (see [`Job::echo`]).
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
        // Neither a paste nor a UI-encoded report may spend the keys' reserve: behind
        // a program that stopped reading, a DEC 1003 motion flood must refuse motion,
        // never the next key.
        let paste = matches!(ev, InputEvent::Paste(..)) || echo.is_none();
        let master = sink.master();
        let (tx, budget) = {
            let mut reg = REG.lock().unwrap_or_else(|p| p.into_inner());
            // Prune sessions whose sink is gone (closed tabs): dropping the stored
            // Sender lets their idle writer thread exit. Only queued input pays it
            // (on unix, input behind a paste).
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
        // Claim the FIFO slot before releasing to the writer: a later keystroke on
        // this same (UI) thread then observes pending > 0 and queues behind us.
        sink.claim_ordered_egress();
        // Claim the arrival here so the UI thread's `note_pty_write` cannot book the
        // channel push as the write. A paste or report claims nothing: Cmd-V's arrival
        // stamp would book a whole backpressured paste as one key_write sample and
        // poison the never-reset `MAX_KEY_WRITE_NS`.
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
                // The UI thread's echo bracket must not arm for this event:
                // its write happens on the writer thread, which arms it there.
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
                    // A receiver that exited must not poison future retries.
                    // Replace only this serializer, never a newer fd owner.
                    let mut reg = REG.lock().unwrap_or_else(|p| p.into_inner());
                    if reg
                        .get(&master)
                        .is_some_and(|entry| Arc::ptr_eq(&entry.budget, &job._reservation.budget))
                    {
                        reg.remove(&master);
                    }
                }
                // Dropping the job releases its reservation. No fallback may
                // write ahead of accepted input or launch an unbounded worker.
                drop(job);
                Err(reason)
            }
        }
    }

    /// The shared ordered/inline tail: a paste, and any event while this sink's
    /// ordered writer holds work (on Windows, every event), is submitted to the
    /// session's FIFO so nothing overtakes accepted input; a refused admission accepts
    /// nothing. Otherwise `ev` is written inline through the interactive egress, which
    /// never parks. Returns the receipt and whether the write ran inline.
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

    /// [`ordered_or_inline`] with the platform's routing fact explicit, so the
    /// Windows arm — every key through the writer — runs under test on unix.
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

    /// A control socket lane's write into a session that is not on screen
    /// (`crate::control`'s background arms): [`ordered_or_inline`]'s route —
    /// behind whatever this session's ordered writer holds, through it always
    /// where a write can block (Windows), otherwise inline through the
    /// non-parking interactive egress — except that a paste with nothing queued
    /// ahead of it is written inline too, into the sink's own spill, whose
    /// drain the paste's reply waits on. Never parks the lane on the program.
    ///
    /// An event handed to the ordered writer comes with its [`Written`]: admission
    /// is not the write, so the lane's reply waits, bounded, for the write's own
    /// receipt, and a write into a dead peer is answered as failed.
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

    /// The tail [`ordered_or_inline_routed`] and [`unparked_observed`] share: `queue`
    /// submits `ev` to the session's FIFO (a refusal accepts nothing),
    /// otherwise it is written inline through the interactive egress.
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

    /// The report egress: mouse, wheel and focus reports from the App's arms, from
    /// outside them ([`super::write_ui_report`]) and from a control lane into a session
    /// not on screen. A report follows the keys' route: while this session's ordered
    /// writer holds anything, and on Windows always, it is encoded here under the
    /// modes the gesture was judged by and queued behind everything before it;
    /// otherwise it is written inline. A report the FIFO does not admit is refused and
    /// never written. The receipt's `egress` feeds the caller's tracking-off fallback.
    pub(crate) fn report_ordered_or_inline(
        term: &Arc<Mutex<Terminal>>,
        modes: &Arc<ModeMirror>,
        sink: &Arc<SinkWriter>,
        ev: &InputEvent,
    ) -> crate::input::EgressReceipt {
        report_ordered_or_inline_routed(ui_writes_can_block(), term, modes, sink, ev)
    }

    /// [`report_ordered_or_inline`] with the platform's routing fact explicit,
    /// so the Windows arm — every report through the writer — runs under test
    /// on unix.
    pub(super) fn report_ordered_or_inline_routed(
        ui_writes_can_block: bool,
        term: &Arc<Mutex<Terminal>>,
        modes: &Arc<ModeMirror>,
        sink: &Arc<SinkWriter>,
        ev: &InputEvent,
    ) -> crate::input::EgressReceipt {
        report_route(ui_writes_can_block, term, modes, sink, ev, false).0
    }

    /// [`report_ordered_or_inline`] for a control lane's mouse report into a session
    /// not on screen: a report handed to the ordered writer comes with its
    /// [`Written`], as [`unparked_observed`]'s events do.
    pub(crate) fn report_unparked(
        term: &Arc<Mutex<Terminal>>,
        modes: &Arc<ModeMirror>,
        sink: &Arc<SinkWriter>,
        ev: &InputEvent,
    ) -> (crate::input::EgressReceipt, Option<Written>) {
        report_route(ui_writes_can_block(), term, modes, sink, ev, true)
    }

    /// The report route's one body; `written` asks for the queued write's [`Written`].
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
        // The seam's own bytes, carried verbatim: the raw-bytes arm writes
        // exactly them, so the writer produces nothing of its own.
        let frame = InputEvent::KeySequence(bytes);
        match submit(term, modes, sink, None, frame, None, None, None, None, tx) {
            Ok(()) => (crate::input::EgressReceipt::deferred_full(), rx),
            Err(_) => (crate::input::EgressReceipt::queue_full(), None),
        }
    }

    /// Test-only: holds this sink's FIFO occupied so the key path takes the ordered
    /// branch of [`ordered_or_inline`], reproducing a key typed while a paste drains
    /// without wedging a real PTY. It claims what a real job claims (one slot on the
    /// sink-local pending counter) through the same `writer_for` registry, so
    /// `is_ordering` answers for the real reason. The guard releases its slot on drop.
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
            // Test pipes reuse raw fd numbers aggressively. Mirror `enqueue`'s
            // stale-session pruning before resolving by that number, or a pin
            // can attach to a dead serializer from the previous fixture while
            // the real enqueue creates a fresh unpinned one.
            reg.retain(|_, s| s.sink.strong_count() > 0);
            writer_for(&mut reg, master, sink).expect("egress-order writer thread");
        };
        sink.claim_ordered_egress();
        OrderingPin { sink: sink.clone() }
    }

    /// Test-only: the jobs and retained bytes `sink`'s serializer holds admitted now
    /// (the job being written included); `(0, 0)` when it has none.
    ///
    /// Decided by sink identity, not fd number: the entry under `sink.master()`
    /// counts only when its `Weak` points at `sink` itself. Fixtures build borrowed
    /// sinks on pipes they close, so a closed fixture's live serializer can stay
    /// registered under a number the next fixture's `pipe()` reuses; pruning dead
    /// sinks cannot drop it. The pointer compare is sound for a dead entry too, since
    /// a `Weak` keeps its allocation.
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

    /// Test-only: the admission's limits: retained bytes, jobs, and the bytes and
    /// jobs a paste or encoded report may use (the rest is the keys' reserve).
    #[cfg(all(test, unix))]
    pub(crate) const fn limits_for_test() -> (u32, u32, u32, u32) {
        (
            LIMITS.bytes,
            LIMITS.jobs,
            LIMITS.paste_bytes,
            LIMITS.paste_jobs,
        )
    }

    /// Exercises the real channel-rejection arms without a stalled child or large
    /// payloads: a zero-capacity channel with no receiving waiter is full, and
    /// dropping its receiver makes it unavailable. The previous serializer is restored
    /// on drop, including after a panic.
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
                // An unavailable enqueue removes its entry itself. Never
                // overwrite a newer serializer created after that failure.
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

        // The registry still owns `tx`, exactly as it does when the final pasted
        // tab closes. A blocking recv would leave this thread alive forever.
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

/// The input/egress contract, driven through the real ordered writer, reply writer
/// and key seam (`app_input/egress_contract_tests.rs`).
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

/// Map the seam's [`input::Egress`] to the reply-bearing [`InputOutcome`]: a failed
/// PTY write becomes `WriteFailed` (→ `ERR write failed`) so a reply-bearing verb is
/// never told OK for bytes that did not land (the input-path reply-fidelity contract).
fn egress_to_outcome(e: input::Egress) -> InputOutcome {
    match e {
        input::Egress::Reported(input::Delivery::Full) | input::Egress::TrackingOff { .. } => {
            InputOutcome::Ok
        }
        input::Egress::Reported(_) => InputOutcome::WriteFailed,
    }
}

/// The answer for a paste the ordered writer refused whole: its queue full now is
/// [`InputOutcome::QueueFull`] (`ERR busy`, retryable as is), the answer every route
/// gives a refusal at the input cap; a paste too large for the queue, or a writer
/// gone, is a failed write, which a retry as is cannot cure.
const fn rejected_outcome(rejected: paste_order::Rejected) -> InputOutcome {
    match rejected {
        paste_order::Rejected::Full => InputOutcome::QueueFull,
        paste_order::Rejected::TooLarge | paste_order::Rejected::Unavailable => {
            InputOutcome::WriteFailed
        }
    }
}

/// [`egress_to_outcome`] for a whole receipt: a refusal at the session's input cap
/// with nothing written is [`InputOutcome::QueueFull`], whichever route took the
/// event (the front tab's seam, a hidden session, a background control lane). One
/// cut short after an accepted prefix (a wheel's later notches) stays a failed
/// write: a retry would send the prefix twice.
pub(crate) fn receipt_outcome(receipt: input::EgressReceipt) -> InputOutcome {
    if receipt.input_queue_full() && receipt.accepted_order().is_none() {
        InputOutcome::QueueFull
    } else {
        egress_to_outcome(receipt.egress)
    }
}

/// Releases (or stacks onto) the elastic-overscroll bounce for `excess_px` of motion
/// the history-end clamp ate (+ past the top, − past the live bottom): resisted 0.3×
/// and negated, clamped within one cell, decaying via the spring (`tick_overscroll`);
/// the peak is presented on this frame. `set_scroll_band` gates the frac on the
/// SmoothScroll policy.
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
                // A bounce already in flight keeps its phase-locked anchor; one
                // that somehow fell behind `now` re-anchors one period out.
                if sp.next_tick() <= now {
                    sp.set_next_tick(now + tick_interval);
                }
            }
            None => {
                let mut sp = crate::scroll_motion::OverscrollSpring::new(
                    impulse.clamp(-max_px, max_px),
                    now,
                );
                // The first decay tick is one panel period out, anchored here
                // (`OverscrollSpring::next_tick`) and advanced phase-locked.
                sp.set_next_tick(now + tick_interval);
                ws.overscroll = Some(sp);
            }
        }
        if let Some(sp) = ws.overscroll.as_ref() {
            ws.scroll_frac_px = sp.sample(now).0.round() as i32;
        }
    }
}

/// How many viewport lines a wheel gesture of `notch_lines` moves:
/// [`input::wheel_platform_lines`] against this window's viewport height (a "one
/// screen at a time" detent is sized like PgUp/PgDn in `input_scroll_view`). Only a
/// `detent` is scaled; a precise device's banked rows are already rows.
///
/// Applied at the consumer, not where `on_mouse_wheel` banks the delta: the seam
/// emits one mouse report per line while an app tracks the mouse, and an app like
/// vim applies its own `mousescroll` per report. Only surfaces scrolling without an
/// app's help owe the platform's distance: this caller and the DEC-1007 alt-scroll
/// arrow synthesis in the seam (pagers).
///
/// Cost: a touchpad that reports in detent units (a Windows precision touchpad, an
/// X11 smooth-scroll valuator) has its sub-detent fractions banked into whole
/// detents upstream, so it moves in 3-line steps; the real fix is a sub-row wheel
/// path. A `PixelDelta` touchpad (macOS, Wayland) is not a detent and tracks 1:1.
///
/// `page_rows` is the viewport height the seam read under its lock
/// ([`input::WheelViewport::rows`]).
fn wheel_viewport_lines(notch_lines: i32, page_rows: u16, detent: bool) -> i32 {
    input::wheel_platform_lines(input::WheelPlatform::Host, notch_lines, page_rows, detent)
}

/// The modifier-independent (unshifted base) logical key of a winit event, for the
/// keybinding lookup, so a binding written as the base key (`cmd+shift+]`, not
/// `cmd+}`) matches however Shift composes the glyph: `key_without_modifiers()` on
/// macOS, X11/Wayland and Windows. The keypad takes `logical_key` instead: it is
/// NumLock-switched, not layout-shifted, and xkb's level-0 keysym is the NumLock-off
/// symbol (keypad 3 would read `PageDown`). `logical_key` is NumLock-aware and agrees
/// with the PTY encoder through `Key::main_block_twin`.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub(crate) fn base_logical_key(ev: &KeyEvent) -> Key {
    use aterm_window::platform::modifier_supplement::KeyEventExtModifierSupplement;
    if ev.location == aterm_window::keyboard::KeyLocation::Numpad {
        return ev.logical_key.clone();
    }
    ev.key_without_modifiers()
}

/// Fallback for platforms without the modifier-supplement extension: the plain
/// logical key, which is what the keypad takes everywhere.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub(crate) fn base_logical_key(ev: &KeyEvent) -> Key {
    ev.logical_key.clone()
}

/// The key a hardcoded field chord (`^A`, `^K`, Alt+C, ...) reads: the
/// [`base_logical_key`], or, for a chord on a non-Latin layout's letter, the
/// physical key's US letter ([`keybinding::chord_layout_twin`]). Russian Ctrl+ф
/// is then `^A` in the find field exactly as the encoder makes it `^A` in the
/// shell. The `[keybindings]` lookups take the base key and the physical key
/// instead, so a binding written in the layout's own letter still wins there.
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
///
/// aterm's one in-grid text-field keymap, shared by the find bar's query, the
/// tab strip's inline rename and the command palette's filter
/// (`app_palette::palette_key`): `^A`/`Home`/`⌘←` to the start, `^E`/`End`/`⌘→` to the
/// end, `^B`/`^F` and `←`/`→` by character, `⌥←`/`⌥→` by word, `^K`/`^U`/`^W`/
/// `⌥⌫`/`⌘⌫` to kill, `^D`/`⌦` forward-delete, all repeating on a held key. Off
/// macOS, `Ctrl+←`/`Ctrl+→` and `Ctrl+⌫`/`Ctrl+⌦` are also word motions (the
/// Windows/GTK convention); Alt stays an alias everywhere.
///
/// Not claimed: `↑`/`↓` (no vertical motion; the find bar uses them for matches)
/// and plain `⌥`+letter (macOS composes characters there).
///
/// The text arm accepts only printable text: `ev.text` carries `"\u{1b}"` for ⎋,
/// `"\r"` for ⏎ and `"\t"` for ⇥, which are commands to the field's owner, not
/// content.
///
/// This is the physical press's projection onto the keymap ([`field_edit_for`]);
/// [`field_edit_action_engine`] is the engine key's, so a field a controller drives
/// with `aterm ctl key` answers each chord as the keyboard does.
pub(crate) fn field_edit_action(
    mods: ModifiersState,
    ev: &KeyEvent,
) -> Option<crate::app_search::SearchEdit> {
    use aterm_types::keyboard::NamedKey as TNamed;
    // The Ctrl arms compare the base key with one ASCII letter, so a base of more
    // than one character can match none of them. A chord on a non-Latin layout's
    // letter reads as its physical key's US letter (`field_chord_key`): Russian
    // Ctrl+ф is `^A` here as the encoder makes it `^A` in the shell.
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

/// [`field_edit_action`] for an ENGINE key: the event a controller's `key`/`ctrl`
/// verb builds (`control_input::parse_key`) and the overlay seam hands a field
/// (`App::input`). The engine `key` is the layout's unshifted key, the Ctrl arms'
/// base as winit's [`base_logical_key`] is, and the text arm types
/// [`crate::input::typed_glyph`], the glyph the encoder would write for it (Shift's
/// capital, the layout's own character). The caller folds the keypad first
/// (`InputEvent::keypad_folded`).
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

/// One keystroke as the field keymap reads it, projected from winit's key model
/// ([`field_edit_action`]) or the engine's ([`field_edit_action_engine`]).
struct FieldStroke<'a> {
    ctrl: bool,
    alt: bool,
    sup: bool,
    /// The named key, read by the motion and deletion arms.
    named: Option<aterm_types::keyboard::NamedKey>,
    /// The key's base-layout character, read by the Ctrl arms (`^A` … `^W`).
    base: Option<char>,
    /// What the key types, read by the text arm.
    text: Option<&'a str>,
}

/// THE field keymap ([`field_edit_action`] says what it binds), over either
/// projection of a keystroke, so the physical and the driven field cannot drift.
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
    // macOS text-field convention: ⌥ = by word, ⌘ = to the end of the line.
    let by_word = alt && !ctrl_down && !sup;
    // Off macOS, Ctrl is the native word modifier for the named-key arms below; Alt
    // stays an alias (also what `find_suspends_action` preserves for this field).
    // The ctrl-Character emacs arms read `base`, which no named key carries, so
    // ^B/^F still move by one character. macOS keeps ⌥ alone.
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

/// A ⌘ (Super) chord on a character key: a command, never text, so an open
/// overlay swallows it instead of typing it into its search
/// (`App::on_key_overlay_mode`).
fn overlay_command_chord(mods: ModifiersState, ev: &KeyEvent) -> bool {
    mods.super_key() && matches!(ev.logical_key, Key::Character(_))
}

/// Whether this is a bare modifier press. A focused text field must neither consume
/// one as content nor treat it as leaving the field (`⇧` before a capital).
fn is_bare_modifier_key(key: &Key) -> bool {
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

/// Whether the hardcoded Cmd (Super) shortcut suite is live: the gate on every
/// `mods.super_key()` arm that claims a chord before the PTY. Live on macOS and
/// Windows; off on Linux, where the desktop owns Super and the seeded `[keybindings]`
/// table spells every default with Ctrl/Alt, so a Super chord reaches the encoder.
/// Explicit `super+…` bindings still resolve above this suite.
pub(crate) const HARDCODED_SUPER_CHORDS: bool = !cfg!(target_os = "linux");

/// The default scrollback chord, if any: Shift + PageUp/PageDown/Home/End → a
/// [`ScrollIntent`], else `None` so the key reaches the PTY encoder. Shift must be
/// the sole chord modifier (Caps/Num Lock never appear in `ModifiersState`). The
/// xterm / Terminal.app convention: Shift forms scroll the view, plain keys reach
/// the application.
///
/// WINDOWS: Shift+Home / Shift+End are stock PSReadLine's SelectBackwardsLine /
/// SelectLine, so there the jumps take Windows Terminal's Ctrl+Shift+Home / End,
/// and Ctrl+Shift+PageUp / PageDown page as Shift+PageUp / PageDown do (which
/// PSReadLine binds to nothing).
fn scrollback_chord(mods: ModifiersState, ev: &KeyEvent) -> Option<ScrollIntent> {
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

/// Whether this press is the OS "show the context menu" chord aterm may claim: the
/// Menu key, or Shift+F10 (the Windows equivalent). Shift+F10 is terminfo `kf22`,
/// which apps are entitled to, so the default `policy` claims the Menu key alone
/// (`tab_menu_chord = "on"` adds Shift+F10). Written over the engine key so `on_key`
/// and the seam (`tab_menu_input_event`) answer alike. Not a `keybinding::Action`: a
/// rebind could shadow the only keyboard route to the menu, and `policy`
/// ([`crate::app_config::TabMenuChord`]) can only give keys back. Deference to a
/// kitty-protocol client is the callers' `!App::front_defers_tab_menu_chord`.
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

/// Whether an open find field suspends a configured `[keybindings]` action so the
/// keystroke drives the query: text- and viewport-level actions are suspended,
/// app-level ones keep working. Horizontal pane focus is suspended too: off macOS its
/// default Alt+←/→ is the field's word motion, and `on_key` resolves bindings above
/// the find gate.
fn find_suspends_action(action: keybinding::Action) -> bool {
    use keybinding::Action as A;
    matches!(
        action,
        A::Copy
            // Select All drives the terminal's selection, which an open find uses to
            // mark its match, so it stays inert, as the menu's SelectAll arm does.
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

/// Classify the terminal-only Emacs navigation chords after layout normalization:
/// `Some(forward)` when the press opens (or repeats) aterm's find, `None` when it
/// stays with its existing owner.
///
/// Bare Super-S/Super-R (when `HARDCODED_SUPER_CHORDS` is enabled) are aterm's in
/// every terminal. Bare Control-S/Control-R are aterm's only where
/// `agent_composer` answers that an agent's composer owns the front prompt
/// ([`App::front_agent_composer`]); anywhere else the foreground program owns
/// them, as in every terminal: bash's readline reverse-i-search (emacs mode's
/// C-r, stepping back with each further press inside it), zle, fish, a REPL,
/// and vim's redo, including an editor an agent handed the terminal to. A
/// bare Control-S reaches the tty first: under its default IXON it is XOFF
/// (output pauses until Ctrl-Q, or until any key where IXANY is set, as on
/// macOS), and only after `stty -ixon` does readline see it as
/// forward-search-history. `agent_composer` is asked only for a bare
/// Control-S/R. Adding Shift, Alt, or both Super+Control leaves the chord
/// with its existing owner. The caller
/// intentionally runs this only after the native-view and configured-keybinding
/// boundaries; an open find bar takes Control-S/R as its own isearch-repeat
/// below them (`App::search_repeat_action`).
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

/// The shipping `SessionFactory::handed_terminal`: whether the program leading
/// foreground group `pgid` has handed the terminal to another program in its
/// group. That is a member reading the leader's own input device
/// ([`aterm_sysprobe::stdin_sharers`]) under a published name
/// ([`crate::session_status::program::program_of`]) that [`hands_the_terminal_off`]:
/// Claude Code's `$EDITOR`, started with inherited stdio and no group of its
/// own. `false` for no group, or where the group cannot be read. Asked only
/// for a bare Ctrl-R/Ctrl-S under an identified agent
/// ([`App::front_agent_composer`]).
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

/// Whether a member named `program` that reads its group leader's input took
/// the terminal from it: any program but an agent ([`aterm_phase::program_of`])
/// or a runtime one runs as ([`aterm_phase::may_host_agent`]). An agent a
/// launcher runs as its child on the launcher's own stdio (a `node` script
/// starting the native binary, a runtime relaunching itself) IS the agent in
/// front, not a handoff.
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

/// A bare submitted-turn boundary. Modified Enter variants belong to the foreground
/// application (e.g. Shift+Enter's multiline composer) and must not cancel the
/// interactive-echo discount. KP_Enter counts as Enter through
/// `InputEvent::keypad_folded`.
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
    /// Single-slot publication of whether a key release for this session can encode
    /// any bytes, sampled by the press path under a lock it already holds:
    /// `(session_id + 1) << 1 | relevant`, so the pair is never read half-updated; `0`
    /// is unsampled. One slot suffices, since a release follows its press.
    /// Thread-local: publisher and consumer are the event-loop thread, and any other
    /// thread sees "unsampled", the fail-safe.
    static RELEASE_RELEVANCE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Publishes the press-time release relevance of `session`.
///
/// `relevant` must be a conservative superset of "a release can produce bytes": the
/// encoder emits a release report only under Kitty `REPORT_EVENT_TYPES`, so a
/// `false` reading proves the encoder yields nothing.
fn publish_release_relevance(session: u64, relevant: bool) {
    let packed = (session.wrapping_add(1) << 1) | u64::from(relevant);
    RELEASE_RELEVANCE.with(|slot| slot.set(packed));
}

/// `Some(false)` iff the last press-path sample for `session` proved a key release
/// encodes nothing; `None` when nothing was sampled for it (ask the engine).
fn sampled_release_relevance(session: u64) -> Option<bool> {
    let packed = RELEASE_RELEVANCE.with(std::cell::Cell::get);
    if packed == 0 || packed >> 1 != session.wrapping_add(1) {
        return None;
    }
    Some(packed & 1 == 1)
}

/// Mints a fresh App-wide kitty-summon ident: bump the shared sequence first, then
/// namespace the value under `tag`. The shared sequence keeps two windows on one
/// session from minting colliding episodes; the tag partitions each summon kind.
///
/// scope-waiver: "App-wide" names an ident namespace, not a safety budget. A second
/// sequence would mint colliding idents (caught by the episode map), never
/// multiply a rate-limited effect.
fn next_kitty_summon_ident(seq: &mut u64, tag: u64) -> u64 {
    *seq = seq.wrapping_add(1);
    tag ^ *seq
}

/// What a press hands the predictor: a typed glyph, or an edit key
/// (`Predictor::predict_edit`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PredictKey {
    Glyph(char),
    Edit(crate::predict::EditKey),
}

/// One pure classification of a committed Key/Text/KeySequence press, computed once
/// at the top of `input_to_session`'s key arm and read by every effect feed on that
/// path (predictor arming, cursor-FX hints, post-egress cosmetic feeds). Pure over
/// the event, and it never gates bytes.
struct PressClass<'ev> {
    /// One classified input intent for the resident pet. This is not a claim
    /// about delivery or changed ink; the post-egress caller admits it once.
    console_input: Option<PetInputKind>,
    /// Predictive-echo candidate: a bare printable key (no ⌃/⌥/⌘) as the glyph
    /// it types, or an edit key the predictor models (Backspace, and a bare
    /// main-block `←` or `→`). The predictor's `predict_mode` gate stays at the
    /// call site.
    predict_candidate: Option<PredictKey>,
    /// Forward-typing direction for the kitty-cursor momentum, independent of the
    /// predictor's mode gate: a visible char, space or newline advances, a backspace
    /// moves back; other keys leave momentum to decay.
    typed_forward: Option<bool>,
    /// Navigation key hint: keys that move the cursor without writing text (arrows,
    /// Home/End, Page Up/Down, readline's Ctrl-A/E/B/F/P/N and Alt-B/F). It tells the
    /// cursor engines the paired cursor move is a scrub, so a jump to line start or
    /// end never erupts a full-width blaze and held arrows lay no fire.
    navigation_key: bool,
    /// Kill key hint: keys that erase a span in one stroke (Ctrl-K, Ctrl-U, Ctrl-W,
    /// Alt-D, Alt/Ctrl-Backspace, forward Delete). The glow engine poofs only when the
    /// hint pairs with a real same-row content shrink (`CursorGlow::note_kill`), so a
    /// kill an app ignores stays silent.
    kill_key: bool,
    /// Backward kills (Ctrl-U/W, word backspaces) leap the caret, so their echo move
    /// rides the nav-hint choreography. Stationary kills (Ctrl-K, Alt-D, forward
    /// Delete) must not arm nav, or the leaked hint eats the next glyph's wake.
    kill_moves: bool,
    /// Word-scale kills (Ctrl-W, Alt-D, Alt/Ctrl-Backspace, forward Delete) license
    /// the word poof; line kills (Ctrl-K/U) keep the clause-scale swoosh. Meaningless
    /// unless `kill_key` is set.
    kill_word: bool,
    /// Line abort for the kitty-command listener: ⌃C, ⌃Z, ⌃\, ⌃J, ⌃M with Ctrl alone,
    /// each ending the line unjudged. Other feeds see plain chords (`brk`); only the
    /// listener tells an abandoned line (fresh) from one the caret left (poisoned).
    line_abort: bool,
    /// The job-control pair inside `line_abort`: ⌃Z and ⌃\. Under a running program
    /// the tty flushes the typed-ahead text, but at a prompt every shell keeps the
    /// half-typed line, so the listener resets on these only when its line is
    /// provably empty (unpoisoned, nothing counted); else `sit␣` could fire inside
    /// `sotsit `.
    job_chord: bool,
    /// ⌃L (Ctrl alone): the screen clear, which prompts and line editors answer by
    /// redrawing the line intact. The listener ignores it, so a clear right before a
    /// command does not deafen it.
    redraw_chord: bool,
    /// Any Enter press, chorded or not (`is_plain_enter` is the bare form).
    enter_like: bool,
    /// A keyed Enter for the armed celebration (`kitty_sing`): the Enter key, or raw
    /// controller bytes ending in a newline. Program output never reaches this path.
    keyed_enter: bool,
    /// The press writes a line terminator and nothing that could erase or move the
    /// text before it (bare/⇧/⌃ Enter, ⌃J / ⌃M, controller text ending in one): the one
    /// boundary across which the predictor keeps a proven line's guesses on glass.
    line_terminator: bool,
    /// Any Tab press (joins the typed-hint disarm set).
    tab_key: bool,
    /// A bare Tab (no Shift, Control, Alt or Super): the completion the hand asked
    /// for, and the only Tab that arms the delivered-insert licence. ⇧Tab (Claude
    /// Code's mode cycle, readline's reverse menu-complete) keeps the gesture class
    /// and arms no insert.
    insert_tab: bool,
    /// A bare ⌃V (Control, no Alt/Super): an app's own clipboard insert (Claude Code
    /// turns it into `[Image #1] `), quoted-insert under a shell. It arms the
    /// delivered-insert licence of unknown width beside the gesture class; its write
    /// is inline, so dispatch is delivery, and an unechoed credit expires unspent.
    paste_chord_key: bool,
    /// Bare printable glyph for the cosmetic feeds: the shifted glyph the encoder
    /// sends (`Key::Character` holds the unshifted base).
    typed: Option<char>,
    /// A committed IME run (CJK, dead keys, Option-compose) — typed text.
    /// `Text` is built only by `on_ime_commit` and the two human `on_key`
    /// fallbacks, never by paste.
    ime: Option<&'ev str>,
    /// A plain (unchorded) Backspace.
    backspace: bool,
    /// A key that edits or moves beyond one glyph (Enter, Tab, Escape, Delete, nav
    /// keys, any modified chord, raw controller bytes): the cosmetic feeds' break
    /// boundary, since a word assembled across it was never typed as a word.
    brk: bool,
    /// A bare modifier or lock key pressed on its own: no typing intent (the first
    /// half of ⌘C, the Shift of a shift-click), so it moves neither the viewport nor the
    /// selection. Classified by key identity, since the modifier snapshot is stale on
    /// exactly this press (macOS winit queues the bare modifier's `KeyboardInput`
    /// before its `ModifiersChanged`). Side effects only: the event still encodes
    /// (Kitty `REPORT_ALL_KEYS_AS_ESC`).
    inert_modifier: bool,
    /// The bare spacebar (`NamedKey::Space`, unchorded), whose typing click is the
    /// comma ([`aterm_effects::trail_sound::SoundKind::Space`]). Key identity only: a
    /// space inside a committed IME run stays part of that run's click.
    spacebar: bool,
    /// A bare Shift press with no Ctrl/Alt/Super already held: the lift cue's witness
    /// ([`aterm_effects::trail_sound::SoundKind::Shift`]); a mid-chord press is silent.
    shift_key: bool,
    /// The glyph itself was shifted (a capital or shifted symbol), as distinct from
    /// `shift_key`; lifts the keystroke's note an octave
    /// ([`aterm_effects::trail_sound::SoundEvent::shifted`]). Read off the same `mods`
    /// the encoder uses, so the sound and the bytes agree; the snapshot is stale only
    /// on the bare modifier's own keydown, never on the shifted glyph after it.
    glyph_shifted: bool,
}

/// Whether committed text can move the terminal cursor. A run of only combining
/// marks, variation selectors or joiners is zero cells wide, and a typed hint for it
/// could be borrowed by an unrelated PTY move. Tab and line movement stay eligible.
fn committed_text_moves_cursor(text: &str) -> bool {
    text.chars().any(|ch| matches!(ch, '\t' | '\n' | '\r')) || committed_text_cells(text, false) > 0
}

/// Exact terminal-cell advance of one committed Text/IME event: the grid's own
/// column walk ([`aterm_grapheme::str_grid_columns_with`]), so the host prices what
/// the cursor will do. Summing scalar widths would overprice flags, skin tones and
/// ZWJ families and underprice VS16 emoji such as `⚠️`, leaving typed credits a
/// later TUI cursor hop could spend; a cluster's presentation width underprices
/// the spacing vowel signs of Devanagari, Tamil or Thai, each a cell of its own.
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

/// The terminal's ambiguous-width setting (single, or the CJK double), as the
/// committed-text pricers hand it to the grid's column walk.
fn ambiguous_width(ambiguous_width_double: bool) -> aterm_types::text_shaping::AmbiguousWidth {
    use aterm_types::text_shaping::AmbiguousWidth;

    if ambiguous_width_double {
        AmbiguousWidth::Double
    } else {
        AmbiguousWidth::Single
    }
}

/// The columns each grapheme of `text` advances the cursor, in order, on the
/// grid's column walk ([`aterm_grapheme::grid_graphemes_with`]): they sum to
/// [`committed_text_cells`].
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

/// The per-grapheme cell widths of one committed Text/IME event, appended to `out`
/// in order: the clusters [`committed_text_cells`] sums, kept apart so a Backspace
/// can price the one glyph it erases
/// ([`crate::cursor_glow::CursorGlow::note_backspace_erasing`]). A run that advances
/// nothing records nothing. A zero-width cluster inside an advancing run records a
/// priced 0, which retires no cell (unlike an unpriced erase, which retires the
/// newest press).
#[cfg(test)]
fn committed_grapheme_cells_into(text: &str, ambiguous_width_double: bool, out: &mut Vec<u16>) {
    if aterm_grapheme::str_width(text) == 0 {
        return;
    }
    out.extend(grapheme_advances(text, ambiguous_width_double));
}

/// The Backspace price memory: the cell widths of the row's erasable tail, in order,
/// so a Backspace can price the one glyph it erases for the glow engine's press ring
/// ([`crate::cursor_glow::CursorGlow::note_backspace_erasing`]); the engine banks a
/// committed IME run as one press at its summed width.
///
/// A typed press appends its widths (one per grapheme); a movement-capable paste
/// appends a priced 0 per grapheme; each Backspace pops the newest entry (an empty
/// memory prices `None`, retiring the whole newest press); any other press closes
/// the memory. Bounded at [`Self::CAP`], oldest out first. Capacity is reused, so
/// steady typing allocates nothing.
#[derive(Default)]
pub(crate) struct ErasePriceMemory(Vec<u16>);

impl ErasePriceMemory {
    /// How many entries the memory keeps: the engine's press bank depth
    /// (`TYPED_STAMP_DEPTH`). A Backspace past it erases something no banked press
    /// can be charged for, and the whole-press fallback retires at most one per erase.
    pub(crate) const CAP: usize = crate::cursor_glow::TYPED_STAMP_DEPTH;

    /// One typed glyph's width, behind everything already priced.
    fn push(&mut self, width: u16) {
        self.0.push(width);
        self.trim();
    }

    /// A committed Text/IME run, one width per grapheme in order, using the
    /// same width rule as [`committed_text_cells`]. Process bounded chunks so
    /// a large IME commit never stores all the widths we immediately discard.
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

    /// `n` priced zeros — a paste's graphemes, each an erase that retires
    /// nothing of the press ring.
    fn push_zeros(&mut self, n: usize) {
        self.0.extend(std::iter::repeat_n(0u16, n));
        self.trim();
    }

    /// The price of the glyph the newest Backspace erases: the newest entry,
    /// or `None` for the engine's whole-press retire.
    fn pop(&mut self) -> Option<u16> {
        self.0.pop()
    }

    /// Close the memory: a press that moved the erase point, took the row
    /// or banked nothing.
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
            // Compare against the full-vector reference: append/pop/clear, including
            // zero-width-only commits and a leading zero-width cluster in an
            // advancing commit.
            for text in ["ab", "\u{301}", "\u{301}日本", long.as_str(), "⚠️z"] {
                committed_grapheme_cells_into(text, double_width, &mut reference);
                if text == long {
                    assert!(reference.capacity() > 2 * ErasePriceMemory::CAP);
                }
                let excess = reference.len().saturating_sub(ErasePriceMemory::CAP);
                reference.drain(..excess);
                memory.price_graphemes(text, double_width);
                assert_eq!(memory.widths(), reference, "{double_width}");
                // At most two chunks are live; allow Vec's geometric growth
                // from an odd capacity left by the earlier typed/pasted mix.
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
    // A committed IME run is typed text. Read off the original event, before the
    // keypad fold rebinds `ev`: this borrow leaves the function in `PressClass`.
    let ime: Option<&str> = match ev {
        InputEvent::Text(t) if !t.is_empty() => Some(t),
        _ => None,
    };
    // The keypad is classified as its main-block twin: `keymap::build_key_input`
    // keeps the keypad identity for the PTY encoders, but every class below is about
    // what the press means. `PressClass` never gates bytes, so the fold cannot reach
    // the encoder.
    // The arrows the predictor models: a bare `←` or `→`, from the main block or
    // the keypad (NumLock off), which the legacy encoder sends alike (`ESC [ D`,
    // `ESC O D` under DECCKM; `encode_navigation_named_legacy` takes
    // `NumpadArrowLeft` with `ArrowLeft`, DECKPAM or not). Bare means bare:
    // Shift, Ctrl, Alt and Super arrows are word motions, selections or the
    // window's, and Hyper and Meta are no chord a line editor reads as a plain
    // arrow. A LOCK is not a chord: Caps Lock and Num Lock ride on every press
    // while they are on (`keymap::lock_modifiers`; Num Lock is on by default on
    // most Windows keyboards), and the legacy encoder drops them, so the arrow
    // still sends `ESC [ D`. Only Kitty's encoder sends either kind otherwise
    // (the lock in the modifier byte, `ESC [ 1 ; 65 D`; the keypad's own code,
    // `CSI 57417 u`), which the press path asks under its hold
    // (`kitty_reports_functional_keys`).
    let bare = |mods: &TMods| {
        mods.difference(TMods::CAPS_LOCK | TMods::NUM_LOCK)
            .is_empty()
    };
    // The LEARNED keys the predictor places at an authenticated prompt: Home,
    // End and Delete of the main block only (the keypad's are other keys under
    // DECKPAM, and the fold below would make them these), bare; and Ctrl-A and
    // Ctrl-E, Ctrl and nothing else but a lock (the legacy encoder sends `SOH`
    // and `ENQ` with Shift too, but a Kitty mode does not, and nothing here
    // needs it). Under a Kitty mode that reports functional keys Ctrl-A goes out
    // as `CSI 97;5u`, which the predictor refuses itself (R-KITTY, from the
    // press's hold).
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
                // Predict the glyph the encoder sends and the shell echoes:
                // `Key::Character` holds the unshifted base, so 'h'+Shift predicts
                // 'H', and German Shift+2 the layout's '"'.
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
    // A control chord the predictor learns: Ctrl-A, Ctrl-E.
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
        // A committed IME run is typed text: its echo advances the caret and can wrap
        // an Ink box like a plain Character, so it arms the re-anchor hint.
        InputEvent::Text(t) if committed_text_moves_cursor(t) => Some(true),
        _ => None,
    };
    // A Super chord is no line-editor motion: ⌘←, ⌘Home and ⌘PageUp are the
    // window's own commands (swallowed when unclaimed).
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
                // Ctrl-P/N join the emacs line motions: history recall is the same
                // suppressed gesture as Up/Down, so both spellings classify alike.
                TKey::Character(c) if ctrl && matches!(c, 'a' | 'e' | 'b' | 'f' | 'p' | 'n') => {
                    true
                }
                TKey::Character(c) if alt && matches!(c, 'b' | 'f') => true,
                _ => false,
            }
        }
        _ => false,
    };
    // (kill_key, kill_moves, kill_word), as documented on `PressClass`.
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
                // Forward Delete erases one character, so it takes the word poof,
                // not the clause swoosh.
                TKey::Named(TNamed::Delete) => (true, false, true),
                _ => (false, false, false),
            }
        }
        _ => (false, false, false),
    };
    // The kitty-command listener's control chords (`PressClass`): Ctrl held, Alt
    // and Super not. Shift is allowed, since ⌃⇧C encodes the same 0x03 as ⌃C.
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
                    // The glyph the encoder sends (`Key::Character` holds the
                    // base); the detector folds case, so KITTY still counts.
                    typed = Some(input::typed_glyph(*c, *mods, *layout_text));
                    // …and the sound's shift witness, from the same snapshot.
                    glyph_shifted = mods.contains(TMods::SHIFT);
                }
                TKey::Named(TNamed::Space) if !chorded => typed = Some(' '),
                TKey::Named(TNamed::Backspace) if !chorded => backspace = true,
                // A chorded Character is an editing, nav or kill chord (Ctrl-U/W/K,
                // Ctrl-A/E, Alt-B/F, …): it rewrote or left the word.
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
                // Anything else (F-keys, lone modifiers, media keys) neither types nor
                // edits.
                _ => {}
            }
        }
        // A committed IME run is typed text (`ime`, read above the keypad fold).
        InputEvent::Text(_) => {}
        // Raw controller byte payloads are not classified typing: break the run.
        InputEvent::KeySequence(_) => brk = true,
        _ => {}
    }
    // One authority shared with the Kitty report gate, so the lists cannot drift.
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
        // A committed IME bundle is one event, including a combining-only
        // edit that changes a glyph without moving the caret.
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
    /// A line-editing press: a plain Backspace, a navigation key, a kill. None opens a
    /// minibuffer, so the glow keeps the hand on its row across them
    /// (`CursorGlow::clear_typed_keeping_hand`).
    fn edits_the_line(&self) -> bool {
        self.typed_forward == Some(false) || self.navigation_key || self.kill_key
    }
}

/// The licence fence one input event draws before its dispatch, the one
/// classification [`App::input`], `on_key` and the control socket
/// (`control_input::control_fence`) all ask:
/// * `None`: a release (it may precede a delayed echo of its press), buttonless
///   pointer motion (decided at its own arm), a bare modifier (no intent; it must not
///   revoke a nav licence stamped a moment earlier), and a press that arms its own
///   licence class at dispatch (a plain typed glyph, plain Enter, a Tab, a bare ⌃V),
///   whose supersede keeps the typed stamps still in flight;
/// * `Some(true)`, keeping the glow's hand on its row
///   ([`App::clear_move_license_keeping_hand`]): a line-editing press or a paste,
///   whose delivered insert licenses its own echo;
/// * `Some(false)`, taking the hand off ([`App::clear_move_license`]): everything
///   else (a chord that may open a minibuffer such as zle's Ctrl-R, Escape, a click,
///   a focus report).
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

/// The engine-neutral event a physical press delivers to the seam under the window's
/// modifier snapshot `mods`: the `keymap::build_key_input` triple as a press, or, for
/// a key the keymap cannot map (a layout-composed glyph), the IME fallback's
/// committed text. `None` for a key that delivers nothing. Built once per press by
/// `on_key`, classified by [`press_fence`] and sent by the dispatch. `option_composes`
/// is [`crate::platform::option_composes`] of the window's Option setting.
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

/// The hardcoded ⌘V (macOS and Windows, where the window owns Super), shared by its
/// arm in `on_key` and the pre-dispatch fence's paste test ([`App::press_pastes`]).
fn hardcoded_paste_chord(mods: ModifiersState, ev: &KeyEvent) -> bool {
    HARDCODED_SUPER_CHORDS
        && mods.super_key()
        && matches!(&ev.logical_key, Key::Character(s) if s.eq_ignore_ascii_case("v"))
}

/// Whether a delivery is the first landing of a physical press or an auto-repeat
/// tick. `KeySequence` and `Text` carry no event type and [`App::repeat_literal`]
/// replays them verbatim, so the phase is passed with the call.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PressPhase {
    /// The press as the user's finger first landed it (and every non-hold event).
    Initial,
    /// An auto-repeat tick of a hold whose press already took custody.
    Repeat,
}

/// The four-way press classification the custody record carries. A repeat, a bare
/// modifier and a release all leave the viewport and selection untouched, so the
/// kind is carried, not inferred. The bits overlap (a Shift key-up is release and
/// inert); [`PressKind::of`] resolves them Release > Repeat > Inert > Typing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PressKind {
    /// A byte-producing press landing for the first time: the one handover.
    Typing,
    /// An auto-repeat tick of a hold whose press already took custody.
    Repeat,
    /// A bare modifier or lock key.
    Inert,
    /// A Kitty `REPORT_EVENT_TYPES` key-up.
    Release,
}

impl PressKind {
    /// Resolves the three overlapping press bits into one kind, in the priority on
    /// [`PressKind`]. Shared by both press seams (`input_to_session` and
    /// [`App::input_to_hidden_session`]) so they name the same event alike.
    pub(crate) fn of(is_release: bool, is_repeat: bool, inert_modifier: bool) -> Self {
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

    /// Whether this press may disturb the user's reading position:
    /// `!is_release && !is_repeat && !inert_modifier`, i.e. `Typing` only.
    pub(crate) fn disturbs(self) -> bool {
        matches!(self, Self::Typing)
    }
}

/// [`note_person_reaching`] for a gesture that reached the program (the
/// seam's own calls name the class: [`note_person_event`]).
#[cfg(test)]
pub(crate) fn note_person(ctx: &crate::SessionCtx, gesture: crate::human_input::Gesture) -> bool {
    note_person_reaching(ctx, gesture, true)
}

/// A person's hands were on `ctx`'s session (typing, pasting, clicking, dragging,
/// scrolling, composing): stamps its person clock
/// ([`crate::human_input::HumanInputStamp`], `human_ms=`) and records the `human`
/// timeline event (`EVENT <local> human`) and the session log's `mark - human`
/// when this opens a burst. One atomic swap per event; the timeline's leaf lock
/// is taken once per burst. `gesture` says whether it was a plain Enter, for the
/// launch witness's marker ([`crate::human_input::Gesture`]).
///
/// Between bursts the session log's turn tracker still hears of every input, for
/// the `by=human` of a turn opening within 5 s of it, through a `try_lock`: an
/// input never waits on the timeline, and one that finds it held is attributed
/// by the next (a person's Enter follows the keys before it).
///
/// Returns whether the session log's readers were owed a wake (a burst's `mark -
/// human` was written), which rang the session's watchers under the timeline
/// lock (`slog_take_wake`).
///
/// `reaches` is whether the gesture reached the program
/// ([`crate::human_input::reaches_program`]): every gesture stamps the
/// person's clock, counts, and writes the burst's `human` row and mark; only
/// one that reached the program can dismiss a box or answer delivered mail
/// in the session log (rulings T1, T39). Its delivery to the log is once per
/// count ([`crate::session_log::SessionLog::person_input`]).
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

/// The timeline half of [`note_person_reaching`], for the gesture `noted`
/// counted: a burst's row and its delivery under the lock, any other's
/// delivery through a `try_lock`.
pub(crate) fn deliver_noted(
    ctx: &crate::SessionCtx,
    noted: crate::human_input::Noted,
    reaches: bool,
) -> bool {
    if noted.burst {
        let mut tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        tl.record("human", String::new());
        let at = tl.slog().now();
        let own = crate::session_log::PersonGesture {
            burst: Some(noted.seq),
            reaches,
        };
        tl.slog_mut().person_input(Some(own), at);
        tl.slog_take_wake()
    } else if let Ok(mut tl) = ctx.timeline.try_lock() {
        let at = tl.slog().now();
        let own = crate::session_log::PersonGesture {
            burst: None,
            reaches,
        };
        tl.slog_mut().person_input(Some(own), at);
        false
    } else {
        false
    }
}

/// [`note_person_reaching`] for the person's gesture `ev` as the seam
/// receives it, classed by the session's mode mirror
/// ([`crate::human_input::reaches_program`]).
pub(crate) fn note_person_event(ctx: &crate::SessionCtx, ev: &InputEvent) -> bool {
    note_person_reaching(
        ctx,
        crate::human_input::Gesture::of(ev),
        crate::human_input::reaches_program(ev, &ctx.modes, crate::input::WheelPlatform::Host),
    )
}

/// The press half of the custody record: which of the four press classes this
/// delivery was, making `RepeatPressIsInert`, `InertPressIsInert` and
/// `ReleaseIsInert` checkable (their post-states are identical). Anchored here, not
/// on [`apply_press_custody`], which carries `SelectionCustody`'s same-named anchors
/// (two machines' same-named actions cannot share one function). Tier-1 drives all
/// four in [`crate::press_custody_conformance`].
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

/// The `PressCustody` transition a press class is, shared by the recorder above and
/// `apply_press_custody`'s promotion so the two name a press alike.
fn press_transition(kind: PressKind) -> CustodyTransition {
    match kind {
        PressKind::Typing => CustodyTransition::TypingPress,
        PressKind::Repeat => CustodyTransition::RepeatPress,
        PressKind::Inert => CustodyTransition::InertPress,
        PressKind::Release => CustodyTransition::ReleaseEvent,
    }
}

/// What a disturbing press does to the terminal's reading position, shared by the
/// seam's press scope and [`App::input_to_hidden_session`] so the two cannot drift.
/// Returns `(scrolled, cleared)` for the caller's change-gated repaint, both `false`
/// for every kind but `Typing`; takes `&mut Terminal` so the caller owns the lock.
/// The two press arms of the `SelectionCustody` machine: `TypingPress` snaps the
/// viewport and drops the selection ("take me to the prompt"); `InertPress` (every
/// other kind) destroys nothing and does not even read the selection. Tier-1 drives
/// both in [`crate::selection_custody_conformance`].
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
    // A reader who aimed up while a rewrap held the history away sits at offset 0 with
    // a held aim; typing brings the aim home too.
    if disturbs && !scrolled && t.grid().reader_aim_held() {
        t.scroll_to_bottom();
    }
    let cleared = disturbs && t.text_selection().has_selection() && {
        t.text_selection_mut().clear();
        true
    };
    // The `changed=` half: only this site knows whether a typing press took anything
    // (`CustodyTransition::always_takes_custody` is `false` for every press class), so
    // the promotion happens here, under the kind's own transition: an inert class that
    // disturbs shows up as `InertPress` moving something.
    if scrolled || cleared {
        t.note_custody_took(press_transition(kind));
    }
    (scrolled, cleared)
}

/// The bits of one classified press the kitty-command listener's cascade reads
/// ([`PressClass`]'s line-editing subset plus the tty's echo verdict), lifted into
/// one value so the cascade is one function with one order
/// ([`feed_trick_listener`]). Pure data; nothing here gates a byte.
#[derive(Clone, Copy)]
struct TrickPress<'ev> {
    /// The tty will swallow this press (canonical no-echo: a password).
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
    /// ⌃U, the one kill the listener reads as a line reset: it moves the caret and is
    /// not word-scale (⌃W and word backspaces are `kill_word`; ⌃K and forward Delete
    /// do not move).
    fn kills_line(self) -> bool {
        self.kill_key && self.kill_moves && !self.kill_word
    }

    /// The press ends the listener's line, judged or not: Enter (a submit, or a
    /// no-echo prompt's reset), controller bytes ending in a newline, an abort chord,
    /// ⌃U. What the host remembered about the line's fire is then stale. A job chord
    /// on a line not provably empty poisons rather than ends it and is counted too,
    /// since a poisoned line has nothing left to confirm.
    fn line_ended(self) -> bool {
        self.enter_like || self.keyed_enter || self.line_abort || self.kills_line()
    }
}

/// The kitty-command listener's own cascade: one press, one feed call, in this
/// order (`TypedPress::feed` tests `brk` first and cannot carry it):
///
/// ```text
/// tty swallows the press      Enter: note_line_reset (the secret's line is
///                             over) — anything else: note_no_echo (poison;
///                             a passphrase must not move the cat)
/// enter_like (⇧⏎ included)    note_submit: judge the open token, confirm a
///                             tentative fire, report a pet-only line
/// keyed_enter, not Enter      raw bytes ending CR/LF: note_line_reset —
///                             the line that ran was never judged (`sit`
///                             typed, `send 'xyz\n'`: the shell got `sitxyz`)
/// ⌃C, ⌃J, ⌃M, ⌃U              note_line_reset
/// ⌃Z, ⌃\                      note_job_chord: a provably empty line stays
///                             fresh, any other is a break (at a prompt every
///                             shell KEEPS the line; under a program the tty
///                             flushed it: unknown either way)
/// ⌃W / ⌥⌫ / ⌃⌫                note_word_kill
/// ⌃K, ⌥D, forward Delete      nothing: on an unpoisoned line the caret is
///                             at its end, so a forward kill removes nothing
/// ⌃L                          nothing: the screen clear redraws the line
///                             intact everywhere
/// brk (Tab, Esc, arrows, …)   note_break (poison)
/// Backspace                   note_backspace (may revoke)
/// a glyph / an IME commit     note_char / note_ime
/// ```
///
/// `rekey` runs first on every press: the listener's line is the line of the
/// session this press went to. The vocabulary is handed over only when the
/// listener's own predicate says this press will consult it (a token closing on a
/// still-pure line), so a plain letter costs no lookup. Passing `None` when the
/// predicate said `true` would fail closed (the token judged unknown), which is
/// why the predicate, not the host, decides.
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
        // The price of never resetting a poisoned line: after Esc in an editor
        // ⌃Z backgrounds, the prompt stays deaf until Enter, ⌃C or ⌃U.
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

/// What the host did with the last kitty-command fire on a window's current typed
/// line (`WindowState::trick_fire`), since a later `Confirm` or `Revoke` names no
/// word. Reset at every visible line end, an empty line and a revoke; keyed by session
/// ([`Self::for_session`]), since the listener starts a new line on a tab switch
/// silently and a stale `Withheld` from tab A would deny tab B's pet-only submit
/// its exit-127 forgiveness.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum TrickLineFire {
    /// No fire on this line yet.
    #[default]
    None,
    /// The fire was admitted: the flash noted (word engine permitting) and
    /// the pet told (if on glass). `addressed` is the listener's verdict,
    /// upgraded to `true` by a confirmation.
    Admitted { session: u64, addressed: bool },
    /// The fire was withheld — the feature is off, or the word is on
    /// `[sparkle_words.tricks] ignore_words` (a real command of the user's).
    Withheld { session: u64 },
}

impl TrickLineFire {
    /// This memory as it stands for a press that went to `session`: itself
    /// when the fire was on that session's line, [`Self::None`] otherwise.
    /// One compare per press, the listener's own `rekey` in miniature.
    fn for_session(self, session: u64) -> Self {
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

/// A tentative kitty command was taken back: the line turned into prose, was aborted,
/// poisoned or backspaced into. The pet's `revoke_trick` drops only a pending
/// tentative latch, so it is forwarded for every revoke; the flash is left alone when
/// its fire was addressed, so a final word's flash finishes. Ends the host's memory
/// of the fire.
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

/// Revokes, on both engines, the cohort of one dispatch that did not reach the wire:
/// a key queued behind a draining paste, a queued or detached paste, a failed inline
/// write. A queued dispatch keeps its press credit (its bytes will still echo, and
/// the licence returns at delivery via the ticket); a failed write loses the credit
/// too, a queued one when the writer thread reports it ([`revoke_failed_delivery`]).
/// The classic trail follows the glow's stamp. Timestamp-matched, so a newer
/// dispatch's licence is untouched.
fn revoke_dispatch(ws: &mut crate::WindowState, at: std::time::Instant, write_failed: bool) {
    if write_failed {
        ws.fx.cursor_mut().glow.revoke_failed_input_at(at);
    } else {
        ws.fx.cursor_mut().glow.revoke_input_hints_at(at);
    }
    ws.fx.cursor_mut().trail.revoke_input_hints_at(at);
}

/// THE FAILED EDGE, applied by the render prelude with the deliveries
/// ([`Deliveries::failed`]): a QUEUED dispatch whose write the writer thread could
/// not make is revoked as a failed inline write is at once, its press credit with
/// it, on the window that dispatched it. Another window fronting the session
/// banked nothing, and a paste's ticket names no window: a swallowed paste arms
/// nothing.
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
/// ([`paste_order::report_ordered_or_inline`]): counted under `input_refused` like a
/// refused key, but silent, since pointer motion over a stalled program would post at
/// the motion rate. The failed delivery still reaches the arm's outcome.
fn note_refused_report(receipt: input::EgressReceipt) {
    if receipt.input_queue_full() {
        crate::metrics::note_input_refused();
    }
}

/// A report the UI thread writes to a session outside [`App::input`] (the focus delta
/// addressed by session id, [`App::write_focus_report`]; the DEC 2031 colour-scheme
/// report, `App::install_colors`), by the one report route
/// ([`paste_order::report_ordered_or_inline`]): behind whatever the ordered writer
/// holds, always through it on Windows, otherwise inline through the non-parking
/// egress. A refusal is counted silently ([`note_refused_report`]).
pub(crate) fn write_ui_report(
    term: &Arc<Mutex<Terminal>>,
    modes: &Arc<ModeMirror>,
    sink: &Arc<SinkWriter>,
    ev: &InputEvent,
) {
    note_refused_report(paste_order::report_ordered_or_inline(term, modes, sink, ev));
}

/// Why a person's input did not reach the session: the cause the "Couldn't send your
/// input" row names, each with its one remedy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputRefusal {
    /// A paste larger than the ordered queue holds even when it is empty.
    PasteTooLarge,
    /// Earlier input is still being written and the queue behind it is full
    /// (a paste, or a key queued behind a draining paste).
    Busy,
    /// The session's input writer was gone; the next input starts a new one.
    Unavailable,
}

impl InputRefusal {
    /// The cause of a refused ordered enqueue, as the queue measured it.
    const fn of_rejected(rejected: paste_order::Rejected) -> Self {
        match rejected {
            paste_order::Rejected::TooLarge => Self::PasteTooLarge,
            paste_order::Rejected::Full => Self::Busy,
            paste_order::Rejected::Unavailable => Self::Unavailable,
        }
    }

    /// The row's one line.
    const fn line(self) -> &'static str {
        match self {
            Self::PasteTooLarge => "the paste is too large \u{2014} paste it in smaller pieces",
            Self::Busy => "earlier input is still being sent \u{2014} try again",
            Self::Unavailable => "nothing was sent \u{2014} try again",
        }
    }
}

impl App {
    /// Writes one DEC 1004 report to `session` ([`Self::recompute_focus_reports`]'s
    /// delta) by the one report route ([`paste_order::report_ordered_or_inline`]), so
    /// a tab switch away from a frozen program never parks the UI thread; a report
    /// with no room is counted silently. Addressed by session id because `App::input`
    /// routes by window, and the session losing the keyboard is no longer front.
    pub(super) fn write_focus_report(&self, session: u64, focused: bool) {
        let Some(live) = self.pool.get(session) else {
            return;
        };
        write_ui_report(
            &live.term,
            &live.ctx.modes,
            &live.ctx.sink,
            &InputEvent::Focus(focused),
        );
    }

    /// A key or paste refused for `session` (the ordered queue could not admit it,
    /// the writer is gone, or the interactive egress met the spill cap): the program
    /// stopped reading, and refusing beats parking the event loop or overtaking
    /// accepted input. Counted (`input_refused`) and said once per session as a
    /// silent Messages row naming the cause ([`InputRefusal`]); no payload is kept.
    fn refuse_input(&mut self, session: u64, why: InputRefusal) {
        crate::metrics::note_input_refused();
        let key = format!("session.input-full.{session}");
        // One row per session: a repeat of its cause posts nothing, and a new
        // cause replaces the line (the same key supersedes the row).
        if self
            .messages
            .live_by_key(&key)
            .is_some_and(|row| matches!(row.msg.detail.as_slice(), [line] if line == why.line()))
        {
            return;
        }
        // Keyed, so a burst of refused keys is one row.
        self.post_message(crate::message_reporters::input_refused(why.line()).key(&key));
    }

    /// Closes every licence term at a newer input boundary. The licence
    /// (docs/design/EFFECTS-LICENSE-REDESIGN.md) asks whether a human just touched the
    /// keyboard; a key this window swallowed is not an answer, so its stamp must not
    /// outlive the boundary. Expires two timestamps and nothing else.
    pub(crate) fn clear_move_license(&mut self, wid: WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.fx
                .cursor_mut()
                .glow
                .clear_typed(std::time::Instant::now());
            ws.fx.cursor_mut().trail.clear_typed();
        }
    }

    /// [`Self::clear_move_license`] for a boundary that cannot take the hand off its
    /// row: a line-editing press ([`PressClass::edits_the_line`]), a paste, or a bare
    /// modifier edge (`on_modifiers_changed`). The glow keeps the hand on its row
    /// (`CursorGlow::clear_typed_keeping_hand`), since none opens a minibuffer.
    pub(crate) fn clear_move_license_keeping_hand(&mut self, wid: WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            let fx = ws.fx.cursor_mut();
            fx.glow.clear_typed_keeping_hand(std::time::Instant::now());
            fx.trail.clear_typed();
        }
    }

    /// The licence fence [`press_fence`] classified: `keep_hand` closes it
    /// keeping the glow's hand on its row
    /// ([`Self::clear_move_license_keeping_hand`]), else taking it off
    /// ([`Self::clear_move_license`]).
    pub(crate) fn fence_move_license(&mut self, wid: WindowId, keep_hand: bool) {
        if keep_hand {
            self.clear_move_license_keeping_hand(wid);
        } else {
            self.clear_move_license(wid);
        }
    }

    /// Whether a physical press becomes a paste in this window (the hardcoded ⌘V,
    /// [`hardcoded_paste_chord`], or a chord `[keybindings]` binds to `paste`, such as
    /// Shift+Insert), so `on_key`'s fence classifies it as a paste ([`press_fence`]).
    /// `base_key` is `on_key`'s lazily normalized binding key, filled here only when a
    /// lookup needs it.
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
    /// ([`crate::licence_fence::owed`]): those recorded on the session it presented
    /// at its last look, each closing the licence as [`Self::fence_move_license`]
    /// does, the hand kept on its row only if none took it off. Called before a
    /// licence is armed (the head of a winit event, the input seam, a controller
    /// batch) and before one is judged (the cursor frame), so a fence lands ahead
    /// of the output its write causes and behind a key the person pressed after
    /// it. Mints nothing and touches no PTY, cadence or rain state.
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

    /// External OS command surfaces (native menu key equivalents, accessibility
    /// actions) can consume input before `on_key`/`App::input`; this gives them the
    /// same licence boundary without a key, cadence heat or terminal bytes.
    pub(crate) fn clear_front_move_license(&mut self) {
        if let Some(wid) = self.frontmost_window {
            self.clear_move_license(wid);
        }
    }

    /// Licence-only ingress fence for native tab-strip wakes: AppKit's custom TabView
    /// consumes mouse and accessibility gestures without a winit pointer event, and a
    /// controller `TabCmd` addresses the front window. Both go through here so the
    /// headless tests drive the production boundary.
    pub(crate) fn clear_tab_surface_move_license(&mut self, window: Option<WindowId>) {
        match window {
            Some(wid) => self.clear_move_license(wid),
            None => self.clear_front_move_license(),
        }
    }

    /// The `App::input` convergence seam: the sole policy site for input egress.
    ///
    /// Bytes come from the source-blind [`input::seam_egress_receipt`], the only
    /// byte-producing reader of `keyboard_mode()` / `mouse_tracking_enabled()` and the
    /// only caller of `encode_key_with_text`, the `encode_mouse_*` family,
    /// `encode_committed_text`, `format_paste` and the focus-report egress. It reads
    /// the relevant mode once per event under one `term_lock` and ends at
    /// `self.sink.write_frame`. (The predictive-echo gate's
    /// `kitty_suppresses_predictive_echo()` below is a read-only display projection.)
    ///
    /// This method adds the viewport, gesture, clipboard and geometry side effects: it
    /// is the only caller of `seam_egress`, `scroll_display`, `reset_blink` and
    /// `apply_term_resize`, and of the press-path viewport snap and selection clear
    /// ([`apply_press_custody`]). Other `snap_to_bottom` callers are listed in
    /// [`crate::input`]'s module doc.
    ///
    /// `src` is recorded for audit and never branched on, so a Human and a Controller
    /// produce identical bytes for the same `InputEvent`
    /// (`input::seam_egress` takes no source).
    ///
    /// `#[track_caller]` books a resize it carries with the calling line
    /// (`site=input`).
    #[track_caller]
    pub(crate) fn input(&mut self, wid: WindowId, ev: InputEvent, src: Source) -> InputOutcome {
        self.input_to_session(wid, ev, src, None, crate::app_input::PressPhase::Initial)
    }

    /// Whether tone-of-typing inference may run at all: the `tone_melody` knob, the
    /// trail-sound config gates (master toggle, nonzero volume), and a live, responsive
    /// trail-audio worker. A headless app, a non-macOS build, or a failed or wedged
    /// audio worker never spends a cycle on the classifier, loads its weights or
    /// buffers chars. Focus and reduced motion are gated at the drain seams.
    fn tone_infer_active(&self) -> bool {
        self.trail_audio.is_live()
            && self.trail_audio.wedged_for().is_none()
            && self.config.tone_melody_or_default()
            && self.config.trail_sounds_or_default()
            && self.config.trail_sound_volume() > 0.0
    }

    /// The `tone` control-socket verb ([`crate::Wake::ToneStatus`]): the front window's
    /// mood tracker, every gate deciding whether it runs, and what the sound path is
    /// stamping. Read-only and headless-safe.
    ///
    /// The mood is audible-only, and a Technical verdict (the most common) sounds like
    /// a broken feature, so each conjunct of [`Self::tone_infer_active`] is reported
    /// separately and `inferences` separates "the model said technical" from "the
    /// model never ran". Reports the window's char count, never its text: it holds
    /// keystrokes the screen never echoed
    /// ([`aterm_effects::tone_infer::ToneTracker::window_chars`]).
    ///
    /// `Err` when there is no focused window, never a fabricated neutral row.
    pub(crate) fn tone_status(&mut self) -> Result<String, String> {
        let Some(wid) = self.frontmost_window else {
            return Err("no focused window".to_string());
        };
        // The classifier runs at the render tick; settle a verdict due but not yet
        // drained before reading.
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
        // The audibility terms, each the same read the seam or `trail status` makes,
        // so the row can refute "sound is broken" itself.
        let now = std::time::Instant::now();
        let focused = self.cursor_fx_focus(wid, ws.focused, now);
        let mode = self.config.motion_mode();
        let shed = self.effective_shed_envelope(mode, now);
        let glow = self.glow_config();
        let serious_sound = self
            .serious_mode_policy()
            .allows(crate::motion::SeriousEffect::TerminalSound);
        // The constructor the `TrailSoundSeam` conformance's verb reader also calls.
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

    /// The `streak [status]` control verb: one line of PRISM WAKE state for the front
    /// window ([`crate::Wake::StreakStatus`]).
    ///
    /// The streak is deliberately subtle (a ~0.10-coverage comet lasting under a
    /// second), so "no rainbow" could be a disabled key, an unfocused window's
    /// demotion, reduced motion, alt-screen or scrollback suppression, an
    /// echo-discounted stream of your own typing, or a resting engine. Each gate is
    /// reported separately so the answer names the one that stopped it.
    ///
    /// `Err` when there is no focused window, never a fabricated neutral row.
    pub(crate) fn streak_status(&self) -> Result<String, String> {
        let Some(wid) = self.frontmost_window else {
            return Err("no focused window".to_string());
        };
        let Some(ws) = self.windows.get(&wid) else {
            return Err("no focused window".to_string());
        };
        let enabled = self.config.output_streak_enabled_or_default();
        let sound = self.config.output_streak_sound_or_default();
        // The motion verdict uses the render tick's focus input (`cursor_fx_focus`), not
        // the raw bit (reported separately as `raw_focused`), or it would misreport a
        // demoted window or one a typed wake is animating.
        let raw_focused = ws.focused;
        let fx_focused = self.cursor_fx_focus(wid, raw_focused, std::time::Instant::now());
        let animate = self
            .motion_policy(fx_focused)
            .animate(crate::motion::MotionEffect::OutputStreak);
        // Both engine families: a split has no single-pane engine, so reporting only
        // that one would say "engine=none" while comets were on screen.
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

    /// Record a typed-"kitty" completion in the Kitty Log (detector:
    /// [`aterm_effects::kitty_summon`]) as one synthetic `head_peek` sighting, in the
    /// same aggregation cell as on-screen "kitty" word-cats. Nothing is presented: the
    /// echoed word grows its own ambient cat. Inert with sparkle words or the feline
    /// family off. Called only when the ledger clock granted
    /// ([`aterm_effects::kitty_summon::TypedHit::records`]), so a rate-limited
    /// completion costs nothing.
    fn record_typed_kitty(&mut self, wid: WindowId, session: u64, now: std::time::Instant) {
        use aterm_effects::kitty_registry::{
            KittyMagic, KittyShownAs, KittySighting, KittyType, TRAIT_BOW, TRAIT_CROWN,
        };
        // The same verdict the screen shows (`App::companion_verdict`: a pinned
        // favourite, else the tenured program cat, else the launch kitty), so the log
        // records the cat actually drawn. Read first: it borrows `self` mutably,
        // before `sparkle` is held.
        let look = self.companion_verdict(wid, session, now).normalized();
        self.ensure_sparkle();
        // Effects master gate: with sparkle words off no cat can draw
        // (`kitty_enabled` requires `sparkle_on`), so nothing is logged, as with
        // ambient sightings.
        let Some(rs) = self.sparkle.as_ref() else {
            return;
        };
        // Feline sub-gate: `[sparkle_words.feline] enabled = false` leaves the master
        // on but the scanner drops every `Class::Feline` match, so no ambient sighting
        // can occur. The typed summon mirrors that: no row for a category the config
        // cannot produce (`feline.log` gates how sightings record, `feline.enabled`
        // whether cats exist).
        if !rs.cfg.feline {
            return;
        }
        // Displayed-trait bits mirror the word renderer's recorder exactly:
        // only the overlay accessories a drawn cat actually carries are counted.
        let traits = match look.accessory {
            Some(aterm_effects::cat_glyphs_gen::CatGlyphId::AccBow) => TRAIT_BOW,
            Some(aterm_effects::cat_glyphs_gen::CatGlyphId::AccCrown) => TRAIT_CROWN,
            _ => 0,
        };
        // The same language chips an on-screen "kitty" earns, from the same lexicon
        // build the drain resolves against. A lexicon missing the word degrades to
        // the empty set ("unknown") rather than skipping the record.
        let langs = rs
            .lexicon
            .scan("kitty", &aterm_lexicon::ScanOptions::default())
            .into_iter()
            .find(|m| matches!(m.class, aterm_lexicon::Class::Feline))
            .map_or(aterm_lexicon::LangSet::EMPTY, |m| m.langs);
        // Fresh `(session, ident)` per recorded summon: the App-wide sequence keeps
        // two windows on one session from colliding, and the tag separates summons
        // from the word renderer's idents. Bumped only when a row is minted.
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
        // `kitty_log_on = false` drains-and-drops here exactly as at the render
        // drain: the ambient cat still shows, nothing is recorded.
        let enabled = self.kitty_log_enabled();
        let discovered = self
            .kitty_log
            .observe(session, [sighting], &rs.lexicon, now, enabled);
        // A first-ever sighting repoints nothing (only a favourite pin changes the
        // cat), so no wake is owed; the settings book repaints on the log's revision.
        let _ = discovered;
    }

    /// Promotes this window's unpinned companion (the tenured program cat if one has
    /// the cursor, else the launch kitty, [`Self::promotable_kitty`]) into the durable
    /// kitty registry and pins it as the companion, which is also how a cat is kept
    /// across launches. Reached from the menu item, the ⇧⌘P palette row and
    /// `aterm-ctl invoke FavouriteKitty`. `wid` is
    /// the window whose cat is promoted and which presents the hello.
    ///
    /// Returns why it declined: `invoke` over the control socket replies, and `OK`
    /// over a refusal would be false. The menu path discards the `Err` at its call
    /// site.
    pub(crate) fn favourite_kitty_checked(
        &mut self,
        wid: WindowId,
        now: std::time::Instant,
    ) -> Result<(), String> {
        use aterm_effects::kitty_registry::{KittyMagic, KittyShownAs, KittySighting, KittyType};
        // Resolve the render-path cache first, or this silent gate would answer
        // "no cats" on an instance that has not drawn a frame yet.
        self.ensure_sparkle();
        // Effects master gate, then the feline sub-gate, as in `record_typed_kitty`:
        // nothing can draw, so nothing may log.
        let Some(rs) = self.sparkle.as_ref() else {
            return Err("effects are off, so there is no cursor cat to pin".to_string());
        };
        if !rs.cfg.feline {
            return Err("[sparkle_words.feline] is off, so cats do not exist".to_string());
        }
        // The promotable kitty, the verdict below the pin rung, not the current
        // verdict: with a pin on record the verdict is that pin, so favouriting it
        // would change nothing. Promoting the cat that would ride without the pin
        // lets the user swap the pin onto it.
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
            // No matched surface (a favourite is a button press, not a word), so the
            // primary language is "unknown".
            langs: aterm_lexicon::LangSet::EMPTY,
            // Neither `for_launch` nor `for_app` mints an accessory, so no
            // overlay was drawn and no displayed-trait bit may be claimed.
            traits: 0,
            look,
            ident,
        };
        self.kitty_log
            .favourite(&sighting, &rs.lexicon, now, enabled);
        if let Some(ws) = self.windows.get_mut(&wid) {
            // A favourite is a reason to change the identity, so it takes
            // `on_collect` (the immediate hello) rather than the identity-preserving
            // `on_summon`.
            ws.fx.companions_mut().cursor_cat.on_collect(now, look);
            // A no-echo prompt repaints nothing on its own — one explicit wake
            // lets the hello's first frame present.
            if let Some(w) = ws.os_window.as_ref() {
                w.request_redraw();
            }
        }
        Ok(())
    }

    /// Puts on a cat from the collection, by name, now: how to switch to a cat other
    /// than the one `favourite_kitty_checked` would pin. `Ok(look)` is the cat now on
    /// the cursor; `Err` says why not (effects or feline off, no such row). The window
    /// side matches `favourite_kitty_checked`.
    pub(crate) fn wear_kitty(
        &mut self,
        wid: WindowId,
        key: &str,
        now: std::time::Instant,
    ) -> Result<aterm_effects::kitty_registry::KittyLook, String> {
        // As in `favourite_kitty_checked`: resolve the render-path cache so the answer
        // depends on the config, not on whether a frame was drawn.
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

    /// Wears the next cat in the collection (View ▸ Next Kitty), in the collection
    /// book's order (first seen, then key), wrapping; with nothing worn it starts at
    /// the first row. `Err` for [`Self::wear_kitty`]'s reasons, or an empty collection.
    pub(crate) fn wear_next_kitty(
        &mut self,
        wid: WindowId,
        now: std::time::Instant,
    ) -> Result<aterm_effects::kitty_registry::KittyLook, String> {
        let (worn, rows) = self.kitty_log.wearable();
        if rows.is_empty() {
            return Err("no cats collected yet".to_string());
        }
        // The row after the worn one, wrapping. An unknown or absent worn key starts at
        // the first row.
        let at = worn
            .as_ref()
            .and_then(|key| rows.iter().position(|row| &row.key == key));
        let next = at.map_or(0, |i| (i + 1) % rows.len());
        let key = rows[next].key.clone();
        self.wear_kitty(wid, &key, now)
    }

    /// `kitty`: the wearable collection, one `key=` row per collected cat, with
    /// `worn=1` on the one on the cursor. The whole roster rather than the sightings
    /// ledger, since what can be worn is what was collected.
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

    /// `kitty wear <key>`: puts on that cat on the frontmost window and says which cat
    /// is on the cursor. The pin is process-wide; the window only decides which
    /// companion presents the hello.
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

    /// The cat "Favourite This Kitty" promotes and its checkmark reports on: the
    /// companion verdict below the pin rung for `wid` (the tenured program cat on
    /// screen there, else the launch kitty). Reads the tenure gate without advancing
    /// it, so the palette resolver and the press agree.
    pub(crate) fn promotable_kitty(
        &self,
        wid: WindowId,
    ) -> aterm_effects::kitty_registry::KittyLook {
        self.windows
            .get(&wid)
            .and_then(|ws| ws.fx.look().tenure().worn().map(|i| i.look))
            .unwrap_or(self.launch_kitty)
    }

    /// Delivers a typed-word reaction to the window's cursor companion: expression
    /// only. The feline arm is silent (the echoed word grows its own cat); the
    /// profanity wince is owed here because a curse typed at a no-echo prompt never
    /// reaches the screen-side `CurseCue` scanner.
    fn react_typed_word(
        &mut self,
        wid: WindowId,
        now: std::time::Instant,
        hit: aterm_effects::kitty_summon::TypedHit,
    ) {
        self.ensure_sparkle();
        // Same master + family gates the ambient sightings respect: with
        // sparkle words or the feline family off, no cat machinery may move.
        let Some(rs) = self.sparkle.as_ref() else {
            return;
        };
        if !rs.cfg.feline {
            return;
        }
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        // `on_curse` is a no-op on a hidden companion (a reaction, never a summon);
        // only a redraw the reaction earned is requested.
        if hit.profanity
            && ws.fx.companions_mut().cursor_cat.on_curse(now, 1)
            && let Some(w) = ws.os_window.as_ref()
        {
            w.request_redraw();
        }
    }

    /// Summon the typed-word dog cameo (the canine arm of
    /// [`aterm_effects::kitty_summon`], past the typed-a-lot gate), gated by the
    /// sparkle master and the canine family. The dog is a visitor: no identity, no
    /// ledger row, no Kitty Log. Each summon's ident from the App-wide summon sequence
    /// seeds its breed and coat, so consecutive summons differ.
    ///
    /// scope-waiver: "App-wide summon sequence" is a monotonic counter read for a
    /// seed, not an enforcer; a second one would only change which breed appears.
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
        // The visit is scoped to the session the keystroke landed in — the
        // renderers only present it while that session fronts their pane.
        ws.fx.companions_mut().dog_cameo_session = Some(session);
        // A no-echo prompt draws no frames on its own; the summon must present.
        if let Some(w) = ws.os_window.as_ref() {
            w.request_redraw();
        }
    }

    /// Delivers a held-key payload to a session no longer frontmost in any window.
    /// Terminal-side semantics (live-viewport snap, selection clear, paste FIFO order)
    /// still apply, but no visible window's animator, predictor or cursor state is
    /// touched.
    ///
    /// A confirmed paste arrives here too: a deferred delivery (the X11 clipboard read,
    /// the Linux banner or macOS sheet answer) is pinned to the session that owned the
    /// gesture, which may no longer front a window. Its bytes still land on it, framed
    /// by the gesture's [`InputEvent::Paste`]; a closed owner gets nothing.
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
        // The seam's custody gate: a hidden session is reached only by a held key, so
        // these are in practice repeats, which must not disturb the reading position.
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
        // Resolved through the shared `PressKind::of`, so both paths name an event
        // alike.
        //
        // The lock is taken unconditionally, once per delivery. The PTY
        // reader may hold it for a whole `process` batch, but for an inert press the
        // critical section is empty, so contention costs a handoff, at the ~30 Hz
        // repeat rate. In exchange a background session's custody record is as real as
        // a front one's, including the three inert classes.
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
                // Refused whole: nothing of the paste was written.
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
        // A press refused by a full input queue reports `WriteFailed` here and
        // is said once, like the visible key arm's ([`App::refuse_input`]).
        let (receipt, _) = paste_order::ordered_or_inline_observed(
            &term, &modes, &sink, &echo, &ev, None, None, completion,
        );
        if receipt.input_queue_full() {
            self.refuse_input(target_session, InputRefusal::Busy);
        }
        receipt_outcome(receipt)
    }

    /// Stamps `session`'s person clock ([`crate::SessionCtx::human_input`]) when `ev`
    /// is a person's gesture ([`crate::human_input::is_person_gesture`] from
    /// [`Source::Human`]). The seam's only use of `src` besides audit. Read only by the
    /// `status` and `text --json` verbs and a controller's own `key|send if-human=<n>`,
    /// which can hold back only that controller's write.
    fn note_person_input(&self, session: u64, ev: &InputEvent, src: Source) {
        if matches!(src, Source::Human) && crate::human_input::is_person_gesture(ev) {
            // A person is present: the warm spare may exist (`spare`).
            self.spares.note_person(std::time::Instant::now());
            if let Some(owner) = self.pool.get(session) {
                // An owed log wake rang the session's watchers in the mark's hold.
                let _ = note_person_event(&owner.ctx, ev);
            }
        }
    }

    /// Routes an input event through the convergence seam, pinning its terminal to
    /// `target_session`: for a physical key hold whose press established session
    /// ownership, so focus or tab changes cannot retarget later repeats, and for a
    /// pointer event addressed to a visible pane other than the front
    /// (`App::pointer_route`). `None` resolves the current front terminal. Only this
    /// shim is `#[track_caller]`, so the resize ledger (`site=input`) and a panic or
    /// `term_lock` hold inside [`Self::input_to_session_inner`] name the caller.
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
        // Resolve the render-path cache before the word detector reads it for the
        // compiled lexicon and scan options; unresolved, it reads as "no vocabulary"
        // on an instance that has not presented. One bool test on the ordinary path.
        self.ensure_sparkle();
        // Every real input advances the automatic-update quiet clock, but a pending
        // overlap buffers byte-producing input rather than revoking on it: keys, text
        // and raw sequences encode against the frozen modes and write to PTY masters
        // that persist across the overlap. A mode-changing escape can sit unread in the
        // queue while encoding uses the pre-overlap modes; the terminal re-converges
        // when the child drains it. The policy is `input_event_handoff_class`, beside
        // `update_handoff_window_event_class` in lib.rs, so the winit event and the
        // `InputEvent` it lowers to are classified alike.
        match crate::input_event_handoff_class(&ev) {
            crate::UpdateHandoffEventClass::Exempt => {}
            crate::UpdateHandoffEventClass::Tolerated => {
                self.note_update_handoff_tolerated_activity();
            }
            crate::UpdateHandoffEventClass::Revoking => self.note_update_handoff_activity(),
        }
        // Audit only: `src` is bound here so its allowed uses (audit, the person stamp)
        // are obvious and a behavioural `match src` would stand out. It never gates
        // bytes; `input::seam_egress` takes no `Source`.
        let _audit = src;
        // A discrete input attempt is an external recovery stimulus: a window parked
        // after exhausting its surface-retry train gets one fresh bounded episode. PTY
        // output never passes this seam, so it cannot fuel a producer-driven loop. Key
        // releases and pointer motion are not new intent.
        if is_present_recovery_stimulus(&ev)
            && let Some(ws) = self.windows.get_mut(&wid)
        {
            // In no-echo or app-owned-keyboard modes this input may produce no pixel
            // and no PTY output, so the recovery edge drives its attempt itself.
            ws.rearm_present(false);
        }
        // Orphan-release pairing at the seam (the engine-key twin of `on_key`'s
        // `consumed_press_keys`, since controller events carry no physical key): a
        // release whose press the overlay gate consumed is swallowed, even after the
        // overlay closed. Checked before the gate so the entry is always removed.
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
        // The licence fence ([`press_fence`], shared with `on_key` and the control
        // socket), so a swallowed key cannot license a later gesture's move.
        if let Some(keep_hand) = press_fence(&ev) {
            self.fence_move_license(wid, keep_hand);
        }
        // Modal overlay: while one owns this window, PTY-bound input is swallowed.
        // Human keys and clicks are gated in `on_key`/`on_mouse_input`; controller
        // input arrives here, gated on modal state only (never `src`). `Resize` and
        // `Focus` still flow.
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
            // An untracked Key release at the gate pairs with a press predating the
            // overlay (it reached the PTY), so it falls through to the egress below and
            // a Kitty `REPORT_EVENT_TYPES` app gets its release. Tracked releases were
            // swallowed above.
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
                // Route the event to the open overlay, so a controller drives it as a
                // human does, then swallow it from the PTY.
                use crate::overlay::OverlayKind;
                // A control client's key into an open overlay is its gesture,
                // never the person's: Take Over and Hand Back refuse under it
                // (`App::person_gesture_refused`).
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
        // Tab context menu: the engine-neutral twin of `on_key_tab_menu_mode` and
        // `on_key`'s chord arm, in the same slot (after the overlay gate, before the
        // rename field), so a controller's `aterm ctl key menu` and the physical Menu
        // key answer alike. A consumed press is recorded in the overlay gate's set so
        // its release is swallowed above.
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
        // The Autopilot menu (`crate::autopilot_menu`): the engine-neutral twin of
        // its keys in `on_key_claude_lights`, so a controller's `aterm ctl key
        // down|up|enter|escape` moves it as a hand does; any other input closes it
        // and goes on. A consumed press is recorded as the tab menu's is.
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
        // Inline rename field: the engine-neutral twin of `on_key_rename_mode`, so a
        // headless instance can drive and end the in-grid editor with controller
        // `key`/`text`/`paste`. A consumed press is recorded in the overlay gate's set
        // so its release is swallowed above.
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
        // Native tab input boundary: the active first-party app owns text, keyboard,
        // paste, pointer and local scrolling. Human and controller events go through
        // its reducer and never leak bytes into the parked terminal session. Focus and
        // Resize continue through the host seam below.
        //
        // A keystroke or text it takes is timed as a terminal's is, from its arrival
        // (read before the reducer runs) to the present of the page's answer, into the
        // same `input_*` ledger (`metrics::note_native_window_input`). A modifier alone
        // and a release answer nothing, so they arm nothing.
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
        // A control client's key or text into a native view is its gesture
        // (an Editor runs only the commands its grant holds).
        if target_session.is_none()
            && self.with_gesture_source(matches!(src, Source::Controller), |app| {
                app.timed_native_input_event(wid, &ev, arrival)
            })
        {
            return InputOutcome::Ok;
        }
        // Presentation side effects follow a current view of the owned session, not
        // the press-time window identity. If no view fronts the session, the
        // hidden-session seam routes the bytes without arming blink, cat, trail or
        // predictor state elsewhere.
        let pointer = matches!(
            ev,
            InputEvent::MouseButton { .. }
                | InputEvent::MouseMove { .. }
                | InputEvent::Wheel { .. }
        );
        let wid = if let Some(target) = target_session {
            // A pointer event is presented by any visible pane of `wid` showing the
            // session (`App::pointer_route`); keyboard payloads keep the front-only
            // rule, since their side effects belong to the window's front terminal.
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
        // The control fences the presenting window owes, before any arm below stamps
        // its licence ([`Self::reconcile_licence`]).
        self.reconcile_licence(wid);
        // Resolve the optional terminal. Native focus has no fallback shell; resize
        // stays a window property, and focus has no DEC-1004 report to emit. The
        // session id rides along: the typed-"kitty" detector keys its keystroke window
        // to it, the same id the render drain hands the Kitty Log.
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
        // The person's stamp ([`crate::human_input`]), before any classification. It
        // gates none of the person's bytes (a controller's `key|send if-human=<n>` uses
        // it to refuse the controller's own write), and controller input is never
        // stamped.
        let completion = completion.filter(|c| {
            self.pool
                .get(session)
                .is_some_and(|s| c.matches(session, &s.ctx))
        });
        self.note_person_input(session, &ev, src);
        // One injected clock sample for every press-like keyboard or paste dispatch;
        // the consuming arm uses this exact stamp so coupled one-shot licences share a
        // freshness origin. Releases and other input take no clock read.
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
            // Keyboard egress.
            ev @ (InputEvent::Key { .. } | InputEvent::Text(_) | InputEvent::KeySequence(_)) => {
                // Blink reset, viewport snap and selection clear apply to both sources,
                // so controller key verbs behave like human typing. A Kitty key release
                // only encodes; a repeat keeps the blink reset but not the snap and
                // clear (`PressKind::disturbs()`).
                let is_release = matches!(
                    &ev,
                    InputEvent::Key { event_type, .. }
                        if matches!(event_type, aterm_types::keyboard::KeyEventType::Release)
                );
                // One clock sample per committed input event, so every coupled effect
                // (cadence, ribbon, cat, predictor, post-write cosmetic feeds) advances
                // from the same instant. `Some` exactly when `!is_release`: a release
                // pays no clock read and runs no press side effect.
                debug_assert_eq!(input_now.is_some(), !is_release);
                // The keypad folded onto its main-block twin, for the cosmetic readers
                // below that ask what a press means rather than what it encodes (as
                // `classify_press` does for itself). Bound once because the rain's
                // reading gate matches the event by hand.
                let folded_gate = ev.keypad_folded();
                // One pure classification of this press (`PressClass`), shared by the
                // pre-egress hint/predictor block and the post-egress cosmetic feeds.
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
                // Sound never gets ahead of terminal egress: the engine may take the
                // bounded cue under the window borrow below, but policy, event
                // construction and host enqueue wait until `ordered_or_inline` returns.
                // At most one of the typed and bare-Shift arms fires per event.
                let mut pending_key_sound = None;
                // Which presses may disturb the reading position (the viewport snap
                // and the typing-deselects clear). Not a release, not an auto-repeat
                // (its press took custody when it landed; re-running at the repeat
                // rate would destroy a scroll or selection made mid-hold), not a bare
                // modifier. Side effects only, never a byte. `KeySequence`/`Text`
                // carry their repeat in [`PressPhase`].
                let is_repeat = phase == PressPhase::Repeat
                    || matches!(
                        &ev,
                        InputEvent::Key { event_type, .. }
                            if matches!(event_type, aterm_types::keyboard::KeyEventType::Repeat)
                    );
                // The one disjoint kind the side-effect gate and the custody record
                // read; it also separates the three inert classes.
                let press_kind = PressKind::of(is_release, is_repeat, inert_modifier);
                // A press the tty will not echo banks nothing (iTerm2's password-mode
                // rule), read off the pty master at the key
                // (`SinkWriter::tty_swallows_input`, one `tcgetattr`): in canonical
                // no-echo mode (`read -s`, `sudo`, an `ssh` passphrase) no typed
                // licence, press credit, insert or gesture licence, or ticket is banked
                // for a program write to spend. Read only for printable presses and a
                // bare Tab / ⌃V, withheld on positive evidence alone; the PTY write,
                // the pet and the cadence are unchanged.
                let tty_swallows = input_now.is_some()
                    && (typed_forward == Some(true) || insert_tab || paste_chord_key)
                    && sink.tty_swallows_input();
                // A bare Shift at that prompt rings no ting, since the ting announces a
                // capital. The same `tcgetattr`, read only for a first-landing bare
                // Shift, withheld on positive evidence alone.
                let shift_lift_withheld = input_now.is_some()
                    && shift_key
                    && press_kind == PressKind::Inert
                    && sink.tty_swallows_input();
                // A press the typed arm withheld for that reason. Its
                // `swallowed_no_echo=` tally is taken after the PTY write, since a
                // press whose write fails never reached the tty.
                let mut swallowed_press = false;
                // Whether the press armed the composer-newline class (read
                // under the term lock below); the delivery ticket mirrors it.
                let mut composer_newline = false;
                // Licences are stamped only on a press-like event; a release can
                // neither grant one nor spend one.
                if let Some(input_now) = input_now {
                    // A bare modifier is inert and leaves the blink phase alone. Gated on
                    // `inert_modifier` only, not on disturbing: an auto-repeat tick is a
                    // key still held, and the cursor stays solid for the whole hold.
                    if !inert_modifier {
                        self.reset_blink(wid);
                    }
                    // Predictive local echo (mosh-style), resolved before the term lock
                    // so a non-printable key never pays for the cursor sample.
                    let pmode = self.predict_mode();
                    let act: Option<PredictKey> = if pmode == crate::predict::PredictMode::Off {
                        None
                    } else {
                        predict_candidate
                    };
                    let pediting = self.config.predictive_echo_editing_or_default();
                    let typed_enter = is_plain_enter(&ev);
                    // A run's first glyph carries the cursor row to the predictor, its
                    // tail guard's reference (an echo that moved the text right of it
                    // proves nothing), and so does every edit key, which the predictor
                    // places only on a row it can read (with editing off it routes
                    // them as before and reads none); read under the press's hold
                    // below. Asked of the predictor here, before the hold, so type-ahead
                    // reads no row; the resident buffer goes back to the window after
                    // the arm.
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
                    // An arrow, Home, End or Delete classified bare that Kitty's
                    // encoder sends as another key (`classify_press`): one with a
                    // lock on (the lock in its modifier byte, `CSI 1;65H`) or the
                    // keypad's arrow (its own `CSI 57417 u` code). The legacy encoder
                    // sends either as the main block's key; the hold below asks which
                    // encoder applies, only for this press. (Ctrl-A and Ctrl-E under
                    // such a mode are the predictor's own refusal, R-KITTY.)
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
                    // An edit key at a tty that will not echo is input the
                    // predictor does not model, as a glyph is there (the no-echo
                    // gate below): `tty_swallows` is read for typed text alone, so
                    // an edit key pays its own `tcgetattr`, and only while
                    // prediction is on.
                    let edit_swallowed =
                        matches!(act, Some(PredictKey::Edit(_))) && sink.tty_swallows_input();
                    // Momentum arms the cursor cat only while the trail master is on and
                    // the style is rainbow kitty. Collection and typed hellos are
                    // activated separately below, independent of this gate. Cached like
                    // `pmode` (the style changes only on a config reload).
                    let kitty_cursor_enabled = match self.kitty_cursor_enabled_cache {
                        Some(n) => n,
                        None => {
                            // The resolved style, not `GlowStyle::parse` of the raw
                            // string: an unrecognized spelling renders the default, and
                            // parsing it would wrongly answer `Lumen`.
                            let n = crate::app_render::ordinary_kitty_cursor_enabled(
                                self.config.cursor_trail_drawn(),
                                self.glow_style(),
                            );
                            self.kitty_cursor_enabled_cache = Some(n);
                            n
                        }
                    };
                    // Key-time typing click availability, resolved here because these
                    // are App-level knobs and the cue below is taken under a window
                    // borrow. Three cheap Option reads.
                    let click_audible = keystroke_click_audible(
                        // The key path's projection closes only on `Inert`,
                        // the exact complement of `is_live()`.
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
                    // The synth policy the click carries, resolved on the App side and only
                    // when a click could sound. `click_audible` covered every mute term,
                    // so the gain is the volume, as `voice_gain` resolves for the frame
                    // drain (a physical keypress satisfies its focus term).
                    let click_synth = click_audible.then(|| KeyClickSynth {
                        style: self.glow_style(),
                        // The trail-gesture voice, the same read the frame drain
                        // makes, so both delivery paths use the same voice.
                        voice: self.trail_gesture_voice(),
                        gain: self.config.trail_sound_volume(),
                        bed: self.config.trail_sound_bed_or_default(),
                        tone_melody: self.config.tone_melody_or_default(),
                    });
                    // The delivered-insert arm's style gate: a bare Tab / ⌃V arms the
                    // insert licence for Rainbow Kitty only, the gate the paste's delivery
                    // edge applies (`cursor_frame`). Resolved on the App side of the
                    // window borrow, and only for those two keys.
                    let insert_gesture_armed = (insert_tab || paste_chord_key)
                        && !tty_swallows
                        && matches!(
                            self.glow_style(),
                            crate::cursor_glow::GlowStyle::RainbowKitty
                        );
                    // Keystroke-priority frame pacing and `input_hint::pending()` signal:
                    // stamped BEFORE acquiring `term_lock_ui` so a PTY reader currently
                    // mid-batch observes `input_hint::pending() == true` at its next slice
                    // boundary (switching from 64 KiB to 8 KiB chunks) and so the
                    // `metrics` input→present slice includes any `term_lock_ui` wait.
                    if !inert_modifier && let Some(ws) = self.windows.get_mut(&wid) {
                        ws.input_hot = true;
                        ws.input_hot_until = Some(input_now + INPUT_HOT_WINDOW);
                        ws.last_key_at = Some(input_now);
                        crate::metrics::note_window_input(&mut ws.present_metrics.pending_input);
                        crate::input_hint::arm();
                    }
                    // One term-lock scope for every press-path terminal touch (snap,
                    // deselect, the predictor's sample), each of which would otherwise
                    // queue behind the PTY reader during floods; the press's last
                    // acquisition, since the seam encodes against the `ModeMirror`.
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
                        // Registered as a UI waiter while blocking, so the reader hands
                        // its next slice boundary to this press. The acquire stays on
                        // the binding line: the L0 freeze guard tracks `t` only from a
                        // same-line acquire.
                        let site = crate::metrics::TermWaitSite::Press;
                        let mut t = crate::term_lock_ui(&term, &ui_waiting, site);
                        // The kind short-circuits before the two reads, so an inert
                        // modifier, repeat or release costs only the one-byte custody
                        // store. Recording inside the guard keeps PTY output from
                        // interleaving between the pre- and post-state a conformance
                        // reads. Shared with `input_to_hidden_session`.
                        let (scrolled, cleared) = apply_press_custody(&mut t, press_kind);
                        // The narrow `REPORT_EVENT_TYPES | REPORT_ALL_KEYS_AS_ESC`
                        // projection, read once here for the predictor's no-echo gate and
                        // the release-relevance publication after this scope.
                        let kitty_owns_keyboard = t.kitty_suppresses_predictive_echo();
                        let sample = act.is_some().then(|| {
                            // The predictor's no-echo gate: the alt screen (vim/less own
                            // the cursor), Kitty REPORT_ALL_KEYS_AS_ESC, or Kitty
                            // REPORT_EVENT_TYPES (Codex's shape: an app-owned composer on
                            // the main screen with full-line repaints, where a ghost could
                            // duplicate the app's text). Disambiguate-only shells stay
                            // eligible, since fish can push that flag at a line-echoing
                            // prompt. A read-only display gate; the seam remains the sole
                            // encoder-feeding reader of the keyboard mode.
                            // `rows` lets the predictor continue a guess across the right
                            // margin at `(row + 1, 0)`; a wrap off the last row declines.
                            //
                            // The content-coordinate generation rides with the cursor:
                            // rows that moved since the predictor last looked (a prompt
                            // scrolled onto the proven bottom row) end its epoch.
                            //
                            // The last field: an arrow with a lock on, or the keypad's,
                            // while Kitty reports functional keys (DISAMBIGUATE), goes
                            // out as another key (the lock in its modifier byte, `ESC [
                            // 1 ; 65 D`; the keypad's code, `CSI 57417 u`), which no
                            // line editor reads as a bare motion, so it is input the
                            // predictor does not model.
                            //
                            // And the integration's word on where the shell's buffer
                            // starts (the authenticated OSC 133 B of the prompt in
                            // front that says so, and which prompt that is, or none),
                            // the editor context the learned keys reach (this session,
                            // the nonce its shell signs with, DECCKM, whether control
                            // chords go out escaped) and whether they do: Ctrl-A is
                            // `CSI 97;5u` under a Kitty mode that reports functional
                            // keys and `CSI 27;5;97~` under modifyOtherKeys level 2.
                            // A few field reads under the hold the press already takes.
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
                                    // The engine's own deferred-wrap flag: a cursor on
                                    // a filled last column is parked there only after
                                    // the terminal wrote that column.
                                    t.grid().pending_wrap(),
                                ),
                            )
                        });
                        // The cursor row, under the hold that read the cursor: what
                        // the run's echoes must leave right of each guess to prove.
                        if let (Some(row), Some((cursor, ..))) = (key_row.as_mut(), sample.as_ref())
                        {
                            crate::predict_seen::seen_row(&t, cursor.row, row);
                        }
                        // Read once under the SAME lock for the PHOSPHOR PgUp/
                        // PgDn reading gate below (no second acquisition).
                        let is_alt = t.is_alternate_screen();
                        // The width the key-time click normalizes its stereo pan
                        // against, the same the frame drain uses. Unconditional; the
                        // `sample` above is gated on the predictor.
                        let term_cols = t.cols();
                        let ambiguous_width_double = t.modes().ambiguous_width_double;
                        // Who takes a plain Enter's line, read for Enter alone: an OSC 133
                        // C (a command running) or the alternate screen puts it in a
                        // foreground program's composer.
                        let shell_executing =
                            typed_enter && crate::quit_safety::shell_executing(&t);
                        // The authenticated prompt a line terminator is pressed at, read
                        // for a terminator alone: the next prompt's first key may start
                        // the line the predictor places Home, End, Ctrl-A, Ctrl-E and
                        // Delete on (`Predictor::note_line_submit_at`), and only at
                        // another prompt.
                        let submit_prompt = line_terminator.then(|| t.prompt_in_front()).flatten();
                        // ATERM_TRACE_SPAWN press sensor (the render trace's gate): one
                        // line per typed-class press, so a ribbon that never lights is
                        // diagnosed at the input seam.
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
                    // Publish whether this press's release can encode anything, so the
                    // release decides without the terminal lock: `kitty_owns_keyboard`
                    // is a conservative superset of `REPORT_EVENT_TYPES`, so `false`
                    // proves it byte-silent, and a release follows the negotiation its
                    // press saw.
                    publish_release_relevance(session, kitty_owns_keyboard);
                    // Change-gated repaint, as the snap/clear helpers gate theirs; an
                    // unconditional wake would cost the hottest typing path.
                    if (scrolled || cleared)
                        && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref())
                    {
                        w.request_redraw();
                    }
                    if let Some(ws) = self.windows.get_mut(&wid) {
                        // A newer press of a different class closes the older licence
                        // term. A typed-glyph press keeps the banked typed stamps (up to
                        // TYPED_STAMP_DEPTH, one spent per echo sweep), so keys that
                        // outrun frames light every cell and program output finds none.
                        let typed_press_supersede = typed_forward == Some(true)
                            && (!enter_like || (enter_like && !typed_enter && is_alt));
                        // Plain Enter and Tab also preserve the bank: each arms its own
                        // class below, and the banked stamps belong to earlier keys. An
                        // inert modifier closes nothing (macOS delivers a bare modifier
                        // keydown as a `KeyboardInput`; revoking would cancel the nav
                        // licence an Option+Left just stamped). A line-editing press
                        // closes every licence term but keeps the hand on its row, since
                        // none opens a minibuffer; every other press takes the hand off.
                        // Navigation re-stamps its own class below (`note_motion`).
                        if inert_modifier {
                            // nothing to supersede
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
                        // Stamp the arrival for the `metrics` verb's input→present slice,
                        // the latency typing feels. The hint armed beside it
                        // (`input_hint::arm`) keeps the PTY reader on short slices
                        // while a human waits on the echo. The write-slice close
                        // (`note_pty_write`) comes after the real egress below, isolating
                        // any blocking write inside this interval.
                        //
                        // Also heat the cadence comet: fast sustained typing compounds
                        // toward ignition, slow typing barely moves it, and pauses decay
                        // it. Clockless (injected `now`). Navigation and kill chords earn
                        // no heat: scrubbing and line kills are not writing.
                        if !navigation_key && !kill_key && !inert_modifier {
                            ws.fx.cursor_mut().cadence.on_keystroke(input_now);
                        }
                        // The fire engine's quench: a Backspace hint cools heat and coals
                        // and classifies the paired echo move as a deletion.
                        if typed_forward == Some(false) {
                            // The glyph it erases, priced by the host: the newest width
                            // of the last typed dispatch (a committed IME run banks
                            // several glyphs; a zero-width cluster is a priced 0), or
                            // `None` for the engine's whole-press retire.
                            let erased = ws.erase_prices.pop();
                            ws.fx
                                .cursor_mut()
                                .glow
                                .note_backspace_erasing(input_now, erased);
                        }
                        let shift_enter_insert = enter_like && !typed_enter && is_alt;
                        // A press the typed arm prices into the memory grapheme by
                        // grapheme (a glyph, a committed IME run, a composer newline),
                        // provided the tty echoes it.
                        let prices_typed_glyphs = typed_forward == Some(true)
                            && (!enter_like || shift_enter_insert)
                            && !tty_swallows;
                        if typed_forward != Some(false) && !inert_modifier && !prices_typed_glyphs {
                            // Any other press closes the memory, since a later Backspace
                            // erases something this dispatch did not price. A typed press
                            // keeps it and appends its widths, so the memory is the row's
                            // erasable tail in order: `abc`, paste `yz`, `d`, ⌫⌫⌫⌫ retires
                            // `d`, nothing for `z`, nothing for `y`, then `c`.
                            ws.erase_prices.clear();
                        }
                        // Composer-newline remains a useful morphology hint for
                        // non-movement consumers, but it is never provenance.
                        if shift_enter_insert {
                            ws.fx.cursor_mut().glow.note_newline_break(input_now);
                            composer_newline = true;
                        }
                        if typed_forward == Some(true) && (!enter_like || shift_enter_insert) {
                            // Width-credit pricing: the glow's anti-stray press budget
                            // counts cells, which only the host knows (a wide CJK glyph
                            // advances two columns, an IME run its summed width); priced
                            // at 1, their echoes would leave a dark notch.
                            let typed_cells: u16 = ime
                                .map(|text| committed_text_cells(text, ambiguous_width_double))
                                .or_else(|| {
                                    typed.map(|ch| committed_char_cells(ch, ambiguous_width_double))
                                })
                                .unwrap_or(1)
                                .max(1);
                            // The typed licence is stamped at the key and spent when the
                            // echo arrives (one hint, one echo). The glyph rides along for
                            // Rainbow Kitty's hero deal (a capital or `!` earns an m1, a
                            // space lays ribbon with no star), since an echo cannot tell
                            // `A` from `a`. Momentum builds from printable typing only.
                            ws.fx.cursor_mut().momentum.on_key(
                                input_now,
                                aterm_effects::cursor_momentum::MOMENTUM_GLOW_TAU_S,
                            );
                            if tty_swallows {
                                // The tally is the only trace of a withheld press; taken
                                // below once the write has landed.
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
                                // Also the widths behind that price, per grapheme, for a
                                // Backspace that erases one of them (`ErasePriceMemory`):
                                // an IME run is banked as one press.
                                match ime {
                                    Some(text) => ws
                                        .erase_prices
                                        .price_graphemes(text, ambiguous_width_double),
                                    None => ws.erase_prices.push(typed_cells),
                                }
                                // The classic trail's stamp goes in lockstep with the
                                // glow's (`CursorTrail::move_licensed`), so a swallowed
                                // press arms neither (pinned by
                                // `a_press_the_tty_will_not_echo_banks_no_credit`).
                                ws.fx.cursor_mut().trail.note_typed(input_now);
                            }
                            // Click at the key, not at the echo: past ~20 ms a click
                            // stops feeling attached to the key, and echoes lag far
                            // more. The engine spends the credit when the echo spawns,
                            // so the character clicks once. The cue is pushed from here,
                            // not the next render tick; one that cannot be taken back
                            // requests a redraw. A capital rings whether Shift or Caps
                            // Lock made it ([`key_click_stamp`]).
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
                                        // Glyph class and rank ride the keyed cue only;
                                        // the echo path has no key to read.
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
                        // The shift lift: the key-time cue for a bare Shift's first landing
                        // (never a repeat or key-up, never mid-chord, not at a password
                        // prompt), through the engine's cue construction. No credit is
                        // banked, and an undeliverable lift is dropped.
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
                                // A bare modifier lands no glyph: class 0,
                                // rank 0.
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
                        // Plain Enter licenses its response: the echo or prompt repaint
                        // after a submit is the keypress landing. `note_return` is
                        // depth-1 and consumed once, so exactly one coalesced response
                        // move is licensed and a program flood after it declines. Into a
                        // foreground program's composer (a command running, or the alt
                        // screen) the hand keeps its row, since Claude Code and Codex
                        // redraw the composer where the next key lands; to a shell, or
                        // when unknown, it leaves (`CursorGlow::note_return`).
                        if typed_enter && !shift_enter_insert {
                            if shell_executing || is_alt {
                                ws.fx.cursor_mut().glow.note_return_keeping_hand(input_now);
                            } else {
                                ws.fx.cursor_mut().glow.note_return(input_now);
                            }
                            ws.fx.cursor_mut().trail.note_return(input_now);
                        }
                        // A Tab or ⌃V is a user gesture: a completion sweep or an app's
                        // clipboard insert (Claude Code's `[Image #1] `) is the
                        // keyboard's own echo, so both engines get a one-shot move
                        // licence (`note_user_gesture`). A bare one is also a delivered
                        // insert of unknown width for Rainbow Kitty
                        // (`insert_gesture_armed`; the write is inline, so dispatch is
                        // delivery). Both are withheld when the tty swallows the press.
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
                        // Navigation re-stamps its own class: the press is the witness of
                        // the jump that follows.
                        if navigation_key {
                            ws.fx.cursor_mut().glow.note_motion(input_now);
                            ws.fx.cursor_mut().trail.note_navigation(input_now);
                        }
                        // Kills after the generic disarm: moving kills need `note_kill`'s
                        // nav classifier; stationary ones license no movement.
                        if kill_key && kill_word {
                            ws.fx
                                .cursor_mut()
                                .glow
                                .note_word_kill(input_now, kill_moves);
                        } else if kill_key {
                            ws.fx.cursor_mut().glow.note_kill(input_now, kill_moves);
                        }
                        // The kitty cursor's momentum drains on a delete at the key;
                        // forward momentum rides the glow's echo pulse instead
                        // ([`CursorGlow::take_momentum_pulse`]), since a keystroke alone
                        // proves no typed text.
                        if typed_forward == Some(false)
                            && (kitty_cursor_enabled
                                || ws.fx.companions_mut().cursor_cat.is_active())
                        {
                            ws.fx.companions_mut().cursor_cat.on_key(input_now, false);
                        }
                        if typed_enter {
                            ws.fx.companions_mut().cursor_cat.on_enter(input_now);
                        }
                        // End the predictive-echo confirmation epoch at every press the
                        // predictor was not handed as a glyph (Enter, ⇧/⌃Enter, ⌃J/M,
                        // Tab, an arrow, an IME commit): a password typed ahead of the
                        // prompt that follows any of them must not inherit the proof.
                        if predict_candidate.is_none() && !inert_modifier {
                            let was_displaying = ws.predictor.is_displaying(input_now);
                            // A line terminator keeps a proven line's guesses painting
                            // until the real echo (flushing would make the command's
                            // tail vanish at Enter). Any other press retires them: it
                            // may erase them (Ctrl+H, Ctrl+W, a remapped kill), and a
                            // guess over an erased cell is a ghost of the erased text.
                            if line_terminator {
                                ws.predictor.note_line_submit_at(submit_prompt, input_now);
                            } else {
                                ws.predictor.note_unmodelled_input(input_now);
                            }
                            // It armed no op: its slice closes at its echo's present.
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
                            // The redraw request delivers any visibility change.
                            if prediction_visibility_requires_redraw(
                                was_displaying,
                                ws.predictor.is_displaying(input_now),
                            ) && let Some(w) = ws.os_window.as_ref()
                            {
                                w.request_redraw();
                            }
                        }
                        // Typing alone keeps the rain weather at CALM, never WORKING.
                        if let Some(rain) = ws.fx.rain_mut() {
                            rain.note_keystroke();
                            // Enter is a turn boundary: a fast response may share the
                            // newline's first present and must not be discounted as echo.
                            if typed_enter {
                                rain.note_signal(
                                    crate::matrix_rain::RainSignal::TurnStart as u32,
                                    4,
                                );
                            }
                            // Reading gate: PgUp/PgDn (keypad included) under a fullscreen
                            // app is scroll-to-read and quiets the rain.
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
                            // A sleeping engine has no armed timer to consume this note,
                            // and a no-echo TUI may not repaint, so one redraw wakes it.
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
                            // Capture before any flush (`reset`, `set_mode`, an invalid or
                            // wrapping `predict_char`, Backspace): a painted ghost stays in
                            // the last frame until an explicit erase redraw.
                            let was_displaying = ws.predictor.is_displaying(now);
                            // The newest op before the press (`armed_since`).
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
                            // A press the tty will not echo (`tty_swallows`: canonical
                            // mode with ECHO clear — `sudo`, `read -s`, a passphrase,
                            // `getpass` — read off the pty at this key) is never a
                            // guess, in ANY mode: the program will not draw it, so a
                            // ghost would be the secret itself, on glass and in every
                            // capture of it. Positive evidence only: a raw-mode program
                            // echoes for itself and stays eligible. An edit key there
                            // (`edit_swallowed`) moves nothing on screen either, and a
                            // line proven before the echo went off is no line it may
                            // be placed on.
                            if no_echo || tty_swallows || edit_swallowed {
                                // A no-echo press arms nothing, but it is still sent,
                                // and the program may echo it after all: an app can
                                // leave the alternate screen before it reads the key,
                                // and the shell behind it then echoes it on the main
                                // screen. Unmodelled input owes that echo, so it
                                // cannot prove a later guess for the same glyph, and
                                // the flush is the stale-epoch hardening (a confirmed
                                // epoch from before entering the app must not let the
                                // first keystroke back on that row display).
                                ws.predictor.note_unmodelled_input(now);
                            } else if chord_arrow {
                                // A modified arrow (a lock in Kitty's modifier byte):
                                // routed as an arrow was before predictive editing.
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
                                // An edit key is accounted for whatever its outcome
                                // (armed, a cancelled glyph, or declined and owed):
                                // never noted as unmodelled input as well.
                                match key {
                                    PredictKey::Glyph(c) => {
                                        let _armed = ws.predictor.predict_glyph(c, &sample, now);
                                    }
                                    PredictKey::Edit(k) => {
                                        let _outcome = ws.predictor.predict_edit(k, &sample, now);
                                    }
                                }
                            }
                            // The key's input slice closes at the first present that
                            // shows its op (`metrics::note_prediction_shown`), or at
                            // its echo's, as before, when it armed none.
                            let armed = ws.predictor.armed_since(op_before);
                            ws.present_metrics.pending_input.tag_newest(armed);
                            #[cfg(any(test, debug_assertions))]
                            crate::predict_trace::press(&ws.predictor, &traced_key, armed, now);
                            if let Some(row) = key_row.take() {
                                ws.predict_key_row = row;
                            }
                            // Repaint when a guess shows or was showing: several
                            // predictor operations flush and return `false`, so keying on
                            // a new guess would strand the old ghost.
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
                    // The release arm (`input_now` is `None` exactly for a release):
                    // the same custody authority as every other press class, which
                    // records the release (`ReleaseIsInert`) and disturbs nothing. One
                    // term-lock acquisition per release, only under Kitty
                    // `REPORT_EVENT_TYPES`.
                    {
                        let site = crate::metrics::TermWaitSite::Press;
                        let mut t = crate::term_lock_ui(&term, &ui_waiting, site);
                        let _ = apply_press_custody(&mut t, press_kind);
                    }
                };
                // While a paste drains for this session, the key goes onto the same
                // per-session FIFO so it cannot overtake the pasted bytes; otherwise
                // the inline fast path (one relaxed atomic decides).
                //
                // The delivery ticket: a key queued behind a paste has its arrival
                // licence revoked below, and the completed write re-stamps it (a Tab /
                // ⌃V as a delivered insert of unknown width, every key as its one
                // class). A press the tty will not echo, and a Kitty key release,
                // ticket nothing, since neither banked anything at dispatch.
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
                        // The gesture half follows the tty's verdict too: a queued Tab
                        // the kernel will not echo re-stamps nothing at delivery.
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
                // The input queue is full: the program stopped reading or the ordered
                // writer could take no more, so this key was refused. Nothing reached
                // the PTY; say so once ([`App::refuse_input`]).
                if receipt.input_queue_full() {
                    self.refuse_input(session, InputRefusal::Busy);
                }
                // The swallowed-press tally counts a no-echo press once it is written
                // or queued; a failed inline write reached no tty.
                if swallowed_press
                    && !outcome.failed()
                    && let Some(ws) = self.windows.get_mut(&wid)
                {
                    ws.fx.cursor_mut().glow.note_typed_swallowed_no_echo();
                }
                // FIFO admission commits an input intent, not delivery; inline input
                // needs an accepted non-empty frame. Neither this note nor the later
                // echo grants contact with text: the pet can only pay attention.
                if outcome == InputOutcome::Ok
                    && (!wrote_inline || receipt.accepted_order().is_some())
                    && let (Some(now), Some(kind)) = (input_now, console_input)
                    && let Some(ws) = self.windows.get_mut(&wid)
                {
                    ws.fx.companion_mut().note_console_input(now, kind);
                }
                // A queued key has not crossed the PTY boundary, and a failed inline
                // write never will, so its arrival-time licences must not be spendable
                // by concurrent program output. This method is one event-loop turn, so
                // no effect tick sees the arm above before this timestamp-matched
                // revoke; the licence returns at delivery through the ticket's key class
                // (`revoke_dispatch` says which engine keeps what).
                if (!wrote_inline || outcome.failed())
                    && let Some(armed_at) = input_now
                    && let Some(ws) = self.windows.get_mut(&wid)
                {
                    revoke_dispatch(ws, armed_at, outcome.failed());
                }
                // The PTY write has returned: close the key→write latency slice, only
                // when the write ran inline and bytes reached the sink. An enqueued key
                // wrote nothing here, and recording would consume the arrival stamp
                // (`note_pty_write` swaps `LAT_KEY_NS` to 0) before the writer thread's
                // real write; a dispatch that encoded nothing is not a key→write.
                // Otherwise disarm, which also keeps the stamp from a later `send`.
                if !is_release {
                    if wrote_inline && receipt.accepted_order().is_some() {
                        crate::metrics::note_pty_write();
                    } else {
                        crate::metrics::clear_key_arrival();
                    }
                }
                // PTY first: decorative audio is resolved and enqueued only after the
                // inline write returned or the FIFO accepted the job, so sound policy
                // can never delay input egress.
                if let Some((cue, synth, cols, glyph_class, rank, at_ms)) = pending_key_sound
                    && let Some(ws) = self.windows.get(&wid)
                {
                    let policy = crate::app_render::TrailSoundPolicy {
                        voice: synth.voice,
                        gain: Some(synth.gain),
                        tone: ws.fx.companions().tone_tracker.effective(synth.tone_melody),
                        bed: synth.bed,
                    };
                    // Stamped at the cue's mint, before the egress (`key_sound_at_ms`),
                    // and pushed only now.
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
                            // The block pre-roll is the audio worker's to
                            // stamp (`MacOut::push_meta`): only it knows
                            // where the in-flight block stands.
                            block_lead_s: 0.0,
                            // The `flow=` heat `trail status` prints.
                            flow: ws.fx.cursor().glow.flow_status().heat,
                        },
                    );
                }
                // Cosmetic typing feeds, after the egress: none steers a byte, and before
                // the write they would put the mood classifier between the keypress and
                // the `write` syscall. Predictor arming and the redraws above stay
                // before it, since they buy perceived latency.
                if let Some(input_now) = input_now {
                    // The word engine's "a human typed" witness, which only the input
                    // path mints (a caret position alone proves nothing). Stamped against
                    // the wire, since its 750 ms budget waits for this key's echo; the
                    // kitty-command flash below stamps against the same moment.
                    let wire_at = if wrote_inline {
                        input_now
                    } else {
                        input_now + ORDERED_EGRESS_WITNESS_GRACE
                    };
                    // TYPED-WORD DETECTOR (feline / profanity / canine): feed
                    // this classified PRESS to the per-window detector.
                    // PRINTED characters — bare
                    // Character/Space plus committed IME Text, the same set
                    // `typed_forward` calls forward — feed the rolling window;
                    // a plain Backspace pops one letter (typo tolerance); and
                    // every key that edits or moves beyond one glyph (Enter,
                    // Tab, Escape, Delete, nav keys, any modified chord, raw
                    // controller byte sequences) CLEARS it — a word assembled
                    // across an editing boundary was never typed as a word.
                    // TYPED INPUT ONLY, never screen content: PTY output and
                    // `cat`ing a file of "kitty" never reach this press path,
                    // and pastes dispatch through a different arm (`Text` is
                    // built only by IME commits, never by paste). Source-
                    // agnostic like every effect on this path — a controller
                    // typing "kitty" summons exactly like a human (the
                    // indistinguishability invariant forbids a `src` branch).
                    // O(1) per key: a bounded 8-slot window + 5-char suffix
                    // compare, so the hottest typing path pays scalar work.
                    let cosmetic_press = aterm_effects::companions::TypedPress {
                        brk,
                        backspace,
                        typed,
                        ime,
                    };
                    // The tone window's bookkeeping runs even while inference is off, so
                    // a re-enable never classifies a window spliced across unseen
                    // editing; only the model is gated (`tone_infer_active`).
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
                            // Tone, sing-along and the summon detector take this
                            // press in one feed (`Companions::note_typed_press`).
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
                    // The cooldown rate-limits the ledger row, not the completion.
                    if summoned.records {
                        self.record_typed_kitty(wid, session, input_now);
                    }
                    // Companion reactions: expression only, never an identity change.
                    if summoned.feline || summoned.profanity {
                        self.react_typed_word(wid, input_now, summoned);
                    }
                    if summoned.canine {
                        self.summon_typed_dog(wid, session, input_now);
                    }
                    // The kitty-command listener (`aterm_effects::typed_tricks`), the
                    // typed-line machine behind `sit␣`, `kitty jump␣`, `good kitty␣`.
                    // It has its own cascade: Enter is judged, ⌃C / ⌃U reset without
                    // judging, Tab and arrows poison, which `brk` cannot distinguish.
                    // Same provenance law (this press only; a paste feeds it a break; no
                    // branch on `src`), and fed always, even with the feature off or in
                    // Serious Mode, so a re-enable never calls an unwatched line pure;
                    // only the events are gated. O(1) per press; the vocabulary is
                    // borrowed only when a token closes on a still-pure line.
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
                        // What an event may do: `[sparkle_words]` × `[..tricks] enabled`
                        // and the Serious Mode policy (a serious window's cat is deaf as
                        // well as still). The ignore list is asked at a fire.
                        let live = self.config.kitty_tricks_enabled()
                            && self
                                .serious_mode_policy()
                                .allows(crate::motion::SeriousEffect::CursorCat);
                        let flash_on = self.sparkle.is_some();
                        match self.windows.get_mut(&wid) {
                            Some(ws) => {
                                // The host's memory of the line's fire is keyed like the
                                // listener's line, per session.
                                ws.trick_fire = ws.trick_fire.for_session(session);
                                // Destructured, not held: `event` borrows the listener's
                                // inline word, and the revoke arm needs the window back.
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
                                        // Admission is decided here, once, and remembered
                                        // for a later confirmation or revoke (neither
                                        // carries the word). The ignore list reads the
                                        // word and its command: `坐下` holds back `猫咪坐下`.
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
                                // The whole submitted line was pet talk: the shell will
                                // say `command not found`, and the cat must not grieve
                                // it, unless a word on the line is on `ignore_words`
                                // (a real command), in which case the failure is theirs.
                                if submit_pet_only
                                    && live
                                    && !matches!(ws.trick_fire, TrickLineFire::Withheld { .. })
                                {
                                    ws.fx.companion_mut().note_trick_submit(input_now);
                                }
                                // The line is over, or provably empty (backspaced or
                                // word-killed to nothing): nothing is left to confirm,
                                // revoke or forgive. The emptiness test matters for a
                                // withheld fire, which no revoke ends: `sit␣` on the
                                // ignore list, erased, then `kitty⏎` is a pet-only line.
                                if press.line_ended() || ws.trick_listener.line_chars() == 0 {
                                    ws.trick_fire = TrickLineFire::None;
                                }
                                pet_note
                            }
                            None => None,
                        }
                    };
                    // The pet hears it only if a cat is already on screen
                    // (`trail_is_kitty_pet`): a typed word never summons, and without
                    // a pet there is only the flash. Latch, never act (`note_trick`,
                    // like `note_bell`); the redraw is owed because only a tick reads
                    // the latch, and a no-echo prompt draws no frame on its own.
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
            // Mouse button: a report when tracking, else the local gesture.
            ev @ InputEvent::MouseButton { .. } => {
                let fronted = self.session_is_front(wid, session);
                self.input_mouse_button(wid, &ev, &term, &modes, &sink, fronted)
            }
            // Mouse motion: a report when tracking, else drag the selection.
            ev @ InputEvent::MouseMove { .. } => {
                let (row, col, side) = if let InputEvent::MouseMove { row, col, side, .. } = ev {
                    (row, col, side)
                } else {
                    unreachable!()
                };
                if let Some(ws) = self.windows.get_mut(&wid) {
                    // `last_mouse_cell` is the pane-local cell `on_cursor_moved` already
                    // published; do not overwrite it with this event's window
                    // coordinates, since a follow-up press or wheel reads it.
                    ws.last_mouse_side = side;
                }
                // A held-button drag with tracking off grows the local selection, in
                // any mode (a drag the app began tracking mid-gesture settles locally).
                // Motion during the front's selection is always the front's
                // (`App::pointer_route`), so this is never another pane's report.
                if self.windows.get(&wid).is_some_and(|ws| ws.selecting) {
                    self.clear_move_license(wid);
                    self.drag_selection(wid, row, col);
                    return InputOutcome::Ok;
                }
                let receipt = paste_order::report_ordered_or_inline(&term, &modes, &sink, &ev);
                note_refused_report(receipt);
                // Full with no accepted bytes means the tracking mode ignores this
                // motion (DEC1000, or buttonless DEC1002), so the hover cannot revoke a
                // key waiting for its echo. Actual reports (accepted, spilled, or queued
                // for the ordered writer) remain boundaries, as do failed attempts. The
                // licence is the front terminal's typed move, so a report to another
                // visible pane leaves it be.
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
            // Wheel: one report per line when tracking, else scroll the viewport.
            ev @ InputEvent::Wheel { .. } => {
                self.input_wheel(wid, session, &ev, &term, &modes, &sink)
            }
            // Explicit, tracking-agnostic scrollback navigation.
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
                    // A paste is input the predictor did not model (and may carry the
                    // line terminator that submits it): what is typed after it must
                    // re-prove its echo before a guess is painted, and the guesses in
                    // flight are retired, since its text is not vetted for erases.
                    ws.predictor.note_unmodelled_input(now);
                    // The kitty-command listener sees a paste as a break: the line now
                    // holds text it never watched being typed, so it is poisoned until a
                    // reset (`echo ` pasted then `sleep␣` typed must not nap the cat). A
                    // paste never fires, and a tentative fire it interrupts is taken
                    // back. Keyed first: the paste went to this session.
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
            // Geometry (range-reject reportable).
            InputEvent::Resize {
                rows,
                cols,
                echo_to_window,
            } => self.input_resize(wid, rows, cols, echo_to_window),
            // A window-pixel resize: the window is asked for the size and the grid
            // follows the platform's `Resized`, as with a drag.
            InputEvent::ResizeWindowPx { width, height } => {
                self.input_resize_window_px(wid, width, height)
            }
            // Focus reporting.
            ev @ InputEvent::Focus(_) => {
                if let Some(ws) = self.windows.get_mut(&wid) {
                    // A focus report is not a keystroke; it closes an older swallowed
                    // typed/delete licence whether or not the child consumes it.
                    ws.fx
                        .cursor_mut()
                        .glow
                        .clear_typed(std::time::Instant::now());
                    ws.fx.cursor_mut().trail.clear_typed();
                }
                // The sole focus-report egress (in `seam_egress`): ESC[I / ESC[O, gated
                // on DEC 1004. The visual blink/cursor-override side effect stays in
                // `on_focus`.
                let receipt = paste_order::report_ordered_or_inline(&term, &modes, &sink, &ev);
                note_refused_report(receipt);
                InputOutcome::Ok
            }
        }
    }

    /// The `InputEvent::MouseButton` arm of [`App::input`]: with tracking on, a press
    /// reports inside `seam_egress`; with tracking off, both sources run the local
    /// left-button selection gesture. `click_count` (1/2/3), `side` and `block` are
    /// carried data: the human handler ran the MULTI_CLICK_MS streak FSM, while a
    /// controller passes an authoritative count without touching `last_press`.
    /// `block` carries the selection-type intent so the seam never reads `self.mods`.
    /// Only the left button selects, and only in the front pane (`fronted`). `ev`
    /// must be `InputEvent::MouseButton { .. }`.
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
        // Carry the gesture fields out before `seam_egress` (which borrows `ev`) for
        // the tracking-off local fallback.
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
        // The local gesture belongs to the front pane: a button addressed to another
        // visible pane whose program does not track the mouse selects nothing.
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
                // The release carries the copy-on-select policy: a scoped-edge-injected
                // gesture (`suppress_copy_on_select == true`) completes the selection
                // without copying to the clipboard. Human and Owner gestures carry
                // `false`.
                self.finish_selection(wid, suppress_copy_on_select);
            }
        }
        receipt_outcome(receipt)
    }

    /// The `InputEvent::Wheel` arm of [`App::input`]: with tracking on, per-line
    /// reports were emitted in `seam_egress`; with tracking off, the local viewport
    /// scrolls by the wheel's lines, times the platform's lines-per-detent when they
    /// are detents ([`wheel_viewport_lines`]), through the smooth glide when the
    /// motion policy permits, and repaints. Positive `display_offset` is older
    /// content. `ev` must be `InputEvent::Wheel { .. }`.
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
        // Rain's alt-screen scroll-quiet gate: an alt-screen wheel becomes PTY bytes
        // whose echo would otherwise intensify the rain while the person reads.
        if let Some(rain) = self.windows.get_mut(&wid).and_then(|ws| ws.fx.rain_mut())
            && modes.alternate_screen()
        {
            rain.note_alt_scroll();
        }
        // A band parked at a history end and pushed further past it takes no lock. A
        // flick to live or to the top keeps delivering precise deltas after the band
        // lands; each would take a blocking `term_lock` in the seam only for the
        // glide to clamp back to the same row.
        //
        // The seam is skipped only when its answer is known: the route, read from the
        // published mode word (`wheel_route_mirrored`, covering tracking, the
        // alternate screen and DEC 1007), is aterm's own viewport, which produces no
        // bytes; and the band is armed on this engine and parked on the edge this
        // delta pushes past, so it clamps against its cached bound as a sub-row delta
        // does (`track_precise_scroll`). A history that grew meanwhile costs one
        // bounce. A rewrap in flight keeps the seam.
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
            // The seam counted detents (one report per line while tracking); the
            // local viewport owes the platform's lines-per-detent for a detent, and
            // a precise device's rows as they are. Zero is meaningful (Windows'
            // "lines to scroll" of 0 means no scroll) and is honoured as no motion.
            // The seam read everything needed under its one lock, so this arm takes
            // no terminal lock of its own.
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
                    // While a rewrap has the history away the glide would clamp this
                    // notch and drop it, so it goes to the reader primitive, which
                    // keeps it as the aim the re-attach lands on.
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
            // The seam reported the delta (an app tracks the mouse, or DEC 1007 alt
            // scroll), so a band still tracking the finger settles to its row now.
            self.release_scroll_track(wid);
        }
        receipt_outcome(receipt)
    }

    /// Whether a precise delta of `dy_px` (+ = into history) pushes a band armed on
    /// `term` and parked on a history end further past it (down at the live bottom, up
    /// at the top). Reads no engine state.
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

    /// Settles every retained smooth-scroll artifact for one window at its intended
    /// whole-row target. Motion-policy edges must use this rather than dropping
    /// [`crate::ScrollGlideState`], which would leave the pinned terminal at an
    /// intermediate row. The glide's terminal may no longer be the front pane. Returns
    /// whether any retained state changed.
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
            // Scrollback can shrink while a glide is retained: land on the intended
            // row when reachable, else at the current boundary.
            let max_row = i64::try_from(term.grid().scrollback_lines()).unwrap_or(i64::MAX);
            // Machine motion since the glide last set the engine row carries the
            // landing row with it, as in `apply_scroll_glide_sample`.
            let machine =
                i64::try_from(term.grid().display_offset()).unwrap_or(i64::MAX) - st.engine_row;
            // Except a glide bound for live, which settles on live.
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
                    // The terminal's live clamp is authoritative if its history
                    // changed between the bound read above and the scroll.
                    break;
                }
            }
        }
        if let Some(window) = redraw {
            window.request_redraw();
        }
        true
    }

    /// Applies the settle only when this window's SmoothScroll policy is Reduced, so
    /// the load-shed latch (which never governs SmoothScroll) cannot land a live glide.
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

    /// Reconciles every retained window after a fact that can change the resolved
    /// SmoothScroll policy (config `motion`, OS accessibility or focus).
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

    /// Install one live OS accessibility sample and reconcile every affected glide in
    /// the same event-loop turn. Returns whether the sampled fact changed.
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

    /// Test wrapper over [`Self::scroll_wheel_animated_at`] on the wall clock, with the
    /// engine facts read under a short lock.
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

    /// Smooth scroll: moves the local viewport by `delta_rows` (positive = into
    /// history) through the ~180 ms ease-out glide when the motion policy permits, and
    /// instantly otherwise (config `motion=reduced`, OS Reduce Motion, an unfocused
    /// window; not the load-shed latch). The scroll pill wakes either way.
    /// Source-blind: it sits below the seam's `TrackingOff` fallback, reached alike by
    /// a wheel notch and a controller `mouse` wheel verb.
    ///
    /// A glide is bound to the engine the wheel targeted ([`crate::ScrollGlideState`]
    /// pins the `Arc`), so chained notches retarget the same ease and a pane change
    /// starts fresh. `viewport` is the seam's `(display_offset, scrollback_lines)`
    /// snapshot ([`input::WheelViewport`]); `None` reads them under a short lock. Base
    /// and clamp are recomputed on every notch, since a target past the top of history
    /// would translate the band against a pinned engine.
    ///
    /// `precise` is a trackpad's signed pixel delta (`PixelDelta`), `None` for a notch.
    /// It moves the band by exactly those pixels with no ease (`Glide::track`), instead
    /// of `delta_rows`, and the band settles to the nearest row `TRACK_REST_MS` after
    /// the last delta: easing under whole-row banking would render a steady finger as
    /// a sawtooth.
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

    /// [`Self::scroll_wheel_animated_with`] on an injected clock: the whole reducer,
    /// with the delta applied at `now`. It reads no clock: every instant it writes is
    /// `now` or an offset of it, except the tick a chained notch keeps
    /// (`next_tick.min(now + tick_interval)`), so tests keep arm and ticks on one
    /// timeline.
    pub(crate) fn scroll_wheel_animated_at(
        &mut self,
        wid: WindowId,
        term: &Arc<Mutex<Terminal>>,
        delta_rows: i32,
        viewport: Option<input::WheelViewport>,
        precise: Option<f64>,
        now: std::time::Instant,
    ) {
        // The pixel domain is the receiving window's cell height, as on the render
        // side (`set_scroll_band`), so a background window's gesture lands on rows.
        let cell_h = self.win_cell_size(wid).1.max(1) as i64;
        let animate = self.smooth_scroll_animates(wid);
        if !animate {
            if !self.windows.contains_key(&wid) {
                return;
            }
            // Reduced motion has no sub-row band: a sub-row-only delta takes no lock.
            if delta_rows == 0 {
                return;
            }
            // Land a retained Full-policy gesture first, then apply this notch.
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
        // A glide still easing another pane's history (the wheel follows the
        // pointer, `App::pointer_route`) lands on its own target first: one window
        // holds one glide, and replacing it would strand that pane mid-ease.
        if self
            .windows
            .get(&wid)
            .and_then(|ws| ws.scroll_glide.as_ref())
            .is_some_and(|st| !Arc::ptr_eq(&st.term, term))
        {
            self.settle_scroll_motion_at_target(wid, now);
        }
        let app_interval = self.frame_interval;
        // Chain onto an in-flight glide of the same engine at the same geometry;
        // anything else starts from the engine's real position.
        let chained = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.scroll_glide.as_ref())
            .filter(|st| Arc::ptr_eq(&st.term, term) && st.cell_h == cell_h)
            .map(|st| (st.engine_row, st.max_rows));
        let same = chained.is_some();
        // The viewport and clamp bound: the seam's snapshot, or for a chained precise
        // delta the glide's own facts, so sub-row trackpad deltas take no lock.
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
        // This window's panel paces its glide.
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
                    // Re-anchored by the apply below to the rest-settle start.
                    next_tick: now,
                });
            }
            let (excess_px, edges) = ws.scroll_glide.as_mut().map_or((0, (false, false)), |st| {
                if viewport.is_some() {
                    // A seam-fed delta (a banked whole row) refreshes the clamp
                    // bound the lock-free sub-row deltas reuse.
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
            // A bounce belongs to the edge that released it: off that edge it ends,
            // or it would show the wrong edge's rubber band.
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
            // A live bounce owns `scroll_frac_px` over the apply's edge frac 0; a
            // 1-3 px excess resists to a zero impulse, so re-present the spring here.
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
            // A chained notch with room to move retargets the ease (a fresh 180 ms).
            ws.overscroll = None;
            if let Some(st) = ws.scroll_glide.as_mut() {
                st.glide.retarget(target_px, now);
                st.max_rows = max_rows;
                // A notch onto a parked tracked band starts easing now; one onto an
                // ease in flight keeps the phase-locked anchor.
                st.next_tick = st.next_tick.min(now + tick_interval);
            }
        } else if target_px == cur_rows * cell_h {
            // Parked at a history end (the clamp ate the whole notch): no glide, but
            // the clamped-away excess releases an elastic bounce.
            ws.scroll_glide = None;
            release_overscroll(ws, want_px - target_px, cell_h, tick_interval, now);
        } else {
            // A fresh ease with room to move: cancel any bounce and arm the glide.
            ws.overscroll = None;
            ws.scroll_frac_px = 0;
            ws.scroll_glide = Some(crate::ScrollGlideState {
                glide: crate::scroll_motion::Glide::new(cur_rows * cell_h, target_px, now),
                term: term.clone(),
                cell_h,
                engine_row: cur_rows,
                max_rows,
                // The first tick is one panel period out, anchored here; a chained
                // notch re-enters through the `same` arm and leaves it alone.
                next_tick: now + tick_interval,
            });
        }
        ws.scroll_pill.touch(now);
        if let Some(w) = ws.os_window.as_ref() {
            w.request_redraw();
        }
    }

    /// One deadline wake of an in-flight glide, from [`Self::service_due_scroll_motion`]
    /// when its anchored tick is due; may take the engine lock.
    pub(crate) fn tick_scroll_glide(&mut self, wid: WindowId, now: std::time::Instant) {
        // The sub-row residual is presented only under a Full SmoothScroll policy; a
        // mid-glide flip to Reduced (e.g. focus lost) snaps whole-row.
        let animate = self.smooth_scroll_animates(wid);
        if !animate {
            // Defensive convergence: if a policy changed without its edge reducer
            // running, the first pending tick lands and drops the glide.
            self.settle_scroll_motion_at_target(wid, now);
            return;
        }
        self.apply_scroll_glide_sample(wid, now);
    }

    /// Presents the glide's sample at `now`: decompose the eased or tracked px into the
    /// whole-row engine offset plus the sub-row residual, move the pinned engine only
    /// on a row change, bank the residual, and drop the state once the sample lands.
    /// Shared by the frame tick and every precise delta.
    ///
    /// The present translate reveals the incoming row from below, so the pairing is
    /// the ceil offset with `frac = offset*cell_h - pos_px`, `frac ∈ [0, cell_h)`
    /// (`scroll_px_decomposition_law`). A `display_offset` other than the row this
    /// glide last set is machine motion (the re-pin after an output batch), and the
    /// glide's frame shifts by it (`Glide::shift`), so the ease stays relative to the
    /// content being read; `settle_scroll_motion_at_target` applies the same rule.
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
                // The glide ends at a detach: while a rewrap has the history out,
                // each wake would grow the reader's aim, so the intent settles once.
                drop(t);
                self.settle_scroll_motion_at_target(wid, now);
                return;
            }
            let cur = i64::try_from(t.grid().display_offset()).unwrap_or(i64::MAX);
            let machine = cur - engine_row;
            // A glide bound for the live bottom keeps the absolute law against a
            // re-pin: row 0 is a destination, not content.
            let live_bound = ws
                .scroll_glide
                .as_ref()
                .is_some_and(|st| st.glide.target_px() == 0);
            // A drop (`machine < 0`: ED 3 emptying history, a reflow) already moved the
            // reader toward live, so it always shifts the frame.
            let offset = if machine < 0 || (machine > 0 && !live_bound) {
                if let Some(st) = ws.scroll_glide.as_mut() {
                    st.glide.shift(machine.saturating_mul(cell_h));
                    // A drop can sink the target below the live bottom, where no row
                    // lies, or leave a tracked band's rest target on live with its start
                    // below. Clamp as the settle twin clamps its landing row: a reader
                    // carried to or past live has arrived; one above it eases on to
                    // live, never below.
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
            // The engine's live clamp is authoritative (history may have shrunk);
            // remember the row it shows so the next apply compares against reality.
            let landed = i64::try_from(t.grid().display_offset()).unwrap_or(i64::MAX);
            if let Some(st) = ws.scroll_glide.as_mut() {
                st.engine_row = landed;
            }
        }
        ws.scroll_frac_px = i32::try_from(frac).unwrap_or(0);
        // Advance the anchored tick: a parked tracked band owes one wake, at its
        // rest-settle start; an ease in flight advances phase-locked to the slot that
        // fired (`phase_locked_effect_deadline`), so a late tick does not shift the
        // cadence.
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
            // A completed glide lands on a whole row: no stale frac survives.
            ws.scroll_frac_px = 0;
        }
        // Keep the pill opaque for the whole glide (the fade hold restarts).
        ws.scroll_pill.touch(now);
        if let Some(w) = ws.os_window.as_ref() {
            w.request_redraw();
        }
    }

    /// Services every armed glide and bounce whose anchored tick is due at `now`:
    /// glides first (a band parked at an edge keeps its glide armed while the bounce
    /// owns `scroll_frac_px`), then bounces. Called on every event-loop wake and at
    /// park time, so a due slot is consumed by the first wake and a park never arms an
    /// instant behind `now`. Returns the windows ticked.
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
            // A band parked at an edge just wrote frac 0, but a live bounce owns
            // `scroll_frac_px`: re-present it at `now` without advancing its anchor.
            if let Some(ws) = self.windows.get_mut(&id)
                && let Some(sp) = ws.overscroll.as_ref()
            {
                ws.scroll_frac_px = sp.sample(now).0.round() as i32;
            }
            ticked.push(id);
            // A glide tick that just started an edge bounce whose wake_deadline is <= now
            // also services that bounce in this pass.
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

    /// One deadline wake of an in-flight elastic-overscroll bounce: sample the spring
    /// at `now`, present its signed sub-cell displacement as `scroll_frac_px`, and drop
    /// the state once it settles. A mid-bounce flip to Reduced motion snaps to rest.
    /// Display-only: the engine is parked at a history end. Called only from
    /// [`Self::service_due_scroll_motion`], after that pass's glide ticks.
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
            // Reduced motion snaps the bounce to rest — whole-row, no residual.
            ws.overscroll = None;
            ws.scroll_frac_px = 0;
            if let Some(w) = ws.os_window.as_ref() {
                w.request_redraw();
            }
            return;
        }
        // Sub-cell displacement, rounded to the nearest device px (the translate
        // consumes integer px). `set_scroll_band` clamps it into `(-cell_h, cell_h)`.
        ws.scroll_frac_px = disp.round() as i32;
        // Advance the anchored tick phase-locked (the glide's law, see
        // `apply_scroll_glide_sample`).
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
            // Settled: drop the spring (disarm) and rest whole-row.
            ws.overscroll = None;
            ws.scroll_frac_px = 0;
        }
        // Keep the pill opaque while the bounce shows (the fade hold restarts).
        ws.scroll_pill.touch(now);
        if let Some(w) = ws.os_window.as_ref() {
            w.request_redraw();
        }
    }

    /// The `InputEvent::ScrollView` arm of [`App::input`]: pure history navigation.
    /// Even under mouse tracking it moves only the local viewport and emits no wheel
    /// bytes, so a read-only edge cannot drive a tracking app through it.
    fn input_scroll_view(
        &mut self,
        wid: WindowId,
        intent: ScrollIntent,
        term: &Arc<Mutex<Terminal>>,
    ) -> InputOutcome {
        if let Some(ws) = self.windows.get_mut(&wid) {
            // Viewport control is not typing. Close the older license before
            // the coordinate class changes.
            ws.fx
                .cursor_mut()
                .glow
                .clear_typed(std::time::Instant::now());
            ws.fx.cursor_mut().trail.clear_typed();
        }
        // Rain's alt-screen scroll-quiet gate: PgUp/PgDn and the scroll verbs are
        // reading intent like the wheel (see `input_wheel`).
        if let Some(rain) = self.windows.get_mut(&wid).and_then(|ws| ws.fx.rain_mut())
            && term_lock(term).is_alternate_screen()
        {
            rain.note_alt_scroll();
        }
        {
            let mut term = term_lock(term);
            let page = i32::from(term.rows()).max(1);
            // The press-custody recorder lives in the `Terminal` scroll methods every
            // arm below calls, so every scroll path records by construction.
            match intent {
                ScrollIntent::Up => term.scroll_display(page),
                ScrollIntent::Down => term.scroll_display(-page),
                ScrollIntent::By(n) => term.scroll_display(n),
                ScrollIntent::Top => term.scroll_to_top(),
                // A non-typing return to live: records `SnapToLive`.
                ScrollIntent::Bottom => term.return_to_live(),
                // Jump to prompt: lift the nearest OSC-133 prompt row above (Prev) or
                // below (Next) the top visible row to the top; none is a no-op.
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
        // The jump stays instant (the controller `scroll` reply reports the applied
        // offset) and drops the front pane's glide tail, sub-row frac and bounce;
        // another pane's glide keeps running.
        self.drop_front_scroll_motion(wid);
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.scroll_pill.touch(std::time::Instant::now());
        }
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
        InputOutcome::Ok
    }

    /// The `InputEvent::Paste` arm of [`App::input`]: a paste, like typing, jumps the
    /// viewport to live. The write runs on the session's ordered writer, off the UI
    /// thread, since a large paste (up to MAX_PASTE_BYTES, 16 MiB) into a program not
    /// reading stdin parks until the consumer drains; teardown severs it
    /// (`SinkWriter::sever_input`). A paste the queue cannot admit is refused
    /// ([`App::refuse_input`], `WriteFailed`). The DEC 2004 framing travels on the
    /// event ([`crate::input::PasteFraming`]), since the bytes are produced later.
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
        // A movement-capable paste is a user gesture whose supersede keeps the banked
        // typed stamps of keys in flight ahead of it; an empty paste arms nothing.
        let movement_capable = matches!(&ev, InputEvent::Paste(text, _)
            if aterm_core::terminal::Terminal::paste_has_payload(text));
        let erase_zeros = match &ev {
            InputEvent::Paste(text, _) if movement_capable => aterm_grapheme::split_graphemes(text)
                .take(ErasePriceMemory::CAP)
                .count(),
            _ => 0,
        };
        // The delivery ticket: the insert's cell width, priced from the text going on
        // the wire (the formatter's sanitizer, then grapheme width under the
        // terminal's ambiguous-width mode), rides the FIFO job; the writer's completed
        // write publishes it and the render prelude arms the delivered-insert licence.
        // One line and the line break after it (a link copied with its line ending)
        // is priced as that line ([`paste_insert_width`]). A body whose width the
        // text cannot say (a Tab, any other line break, a body past the price probe)
        // is an insert of unknown width, bounded by the gesture cap: a same-row echo
        // within it (Claude Code's `[Pasted text #1 +N lines] `, a shell's first
        // line) is admitted, a multi-row echo declines.
        let insert = match &ev {
            InputEvent::Paste(text, _) if movement_capable => {
                let ambiguous_width_double = term_lock(term).modes().ambiguous_width_double;
                Some(paste_insert_width(text, ambiguous_width_double))
            }
            _ => None,
        };
        // A paste into canonical no-echo mode (`read -s`, `sudo`) is never echoed, so
        // no insert licence is armed (the press path's `tty_swallows_input` verdict).
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
        // Enqueue on the session's ordered FIFO; failed admission accepts nothing. A
        // large paste carries a meter: the band shows its progress once it outlasts
        // the grace, and `Stop paste` drops the rest.
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
            // The Backspace price memory: each pasted grapheme (line breaks included,
            // since ZLE, readline and Claude Code keep a pasted newline as one
            // erasable unit) is a priced 0, so a later Backspace over it retires
            // nothing of the typed press ring.
            ws.erase_prices.push_zeros(erase_zeros);
            // The up-strum: a paste's one sound, from a human's Cmd-V or an agent's
            // `paste` alike, voiced by the music box alone.
            if ws.fx.cursor_mut().glow.cue_paste(input_now)
                && let Some(w) = ws.os_window.as_ref()
            {
                w.request_redraw();
            }
        }
        // FIFO admission is not delivery: revoke the gesture stamped above
        // (timestamp-matched) until the bytes land; the render prelude stamps the
        // delivered-insert licence from the write's receipt.
        if movement_capable && let Some(ws) = self.windows.get_mut(&wid) {
            revoke_dispatch(ws, input_now, false);
            // One accepted paste gesture cancels curiosity. It is not a
            // claim that the asynchronous write landed or changed the grid.
            ws.fx
                .companion_mut()
                .note_console_input(input_now, PetInputKind::Paste);
        }
        InputOutcome::Ok
    }

    /// The `InputEvent::Resize` arm of [`App::input`] (range-reject reportable).
    /// `echo_to_window` picks the apply path by where the geometry came from, never by
    /// `Source`: the control `resize` verb echoes the new size to the window
    /// (`apply_grid_resize` -> `request_inner_size`), while the winit `Resized`
    /// handler applies only term, PTY and framebuffer (`apply_term_resize`) so it never
    /// fights an edge drag. A `Resized` for a shared (Cmd-Shift-O) session is driven to
    /// the element-wise min across co-viewers inside `apply_term_resize`.
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
        // The native editor's caret reveal depends on renderer geometry, so reconcile
        // after the rows/cols update so no view keeps the old row-capacity guess.
        let target = if echo_to_window {
            self.frontmost_window.unwrap_or(wid)
        } else {
            wid
        };
        let _ = self.reconcile_active_editor_viewport(target);
        InputOutcome::Ok
    }

    /// The [`InputEvent::ResizeWindowPx`] arm of [`App::input`]: asks the OS window for
    /// an inner size and applies nothing; grid, PTY and swapchain follow the platform's
    /// `Resized` like an edge drag, so the control socket reaches the width throttle,
    /// coalescing and trailing settle ([`Self::input_resize`] applies the grid first).
    /// No `MAX_GRID_*` check: the window's answer shows in the `Resized` that follows.
    /// Zero in either axis is rejected; headless is a no-op.
    fn input_resize_window_px(&mut self, wid: WindowId, width: u32, height: u32) -> InputOutcome {
        if width == 0 || height == 0 {
            return InputOutcome::RangeRejected;
        }
        // The control verb follows the front window, matching `apply_grid_resize`.
        let target = self.frontmost_window.unwrap_or(wid);
        // A driven resize owns the geometry from here: the launch-time initial
        // frame settle must not re-assert the attach grid over it.
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
                // The `Resized` handler's metrics, since no `Resized` will arrive.
                crate::metrics::note_resize_arrival();
                if let Some(ws) = self.windows.get_mut(&target) {
                    ws.win_px = Some(applied);
                    // The trail's resize licence reads this like a drag's `Resized`.
                    ws.last_resized_event_at = Some(std::time::Instant::now());
                }
                self.sync_surface_to_window(target);
                self.on_resize(target, applied);
                w.request_redraw();
            }
        }
        InputOutcome::Ok
    }

    /// Seam-internal left-press gesture dispatch for both sources (the tracking-off
    /// branch of `InputEvent::MouseButton`). `click_count` is authoritative and
    /// `last_press` is not touched: the human handler owns the streak. Source-blind:
    /// the selection type comes from the event's `block`, not `self.mods`, so a held
    /// human Alt cannot leak into a controller press.
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

    /// Records that this key's press was consumed by a GUI gate (no PTY press report),
    /// so [`on_key`]'s release branch swallows the matching release instead of leaking
    /// an orphan Kitty `REPORT_EVENT_TYPES` release. Keyed on the physical key, stable
    /// whatever the modifiers do. A repeat never notes: a chord formed mid-hold
    /// (PageUp held, then Shift) would otherwise swallow a release the app is owed.
    fn note_consumed_press(&mut self, wid: WindowId, ev: &KeyEvent) {
        self.note_consumed_press_key(wid, ev.physical_key, ev.repeat);
    }

    /// The field-level core of [`note_consumed_press`], split out for unit tests, since
    /// a winit `KeyEvent` cannot be constructed there (its `platform_specific` field is
    /// `pub(crate)`).
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
        if repeat {
            return; // a repeat must not (re-)decide the release disposition
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
    /// Focus, tab, modifier and layout state may all change before the release,
    /// which therefore follows this press-time route.
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

    /// Record and deliver literal text/raw bytes as one immutable physical-key
    /// episode. Unlike an encoded key press it has no CSI-u release peer, but
    /// any auto-repeat must retain this exact session and payload: `event`, or
    /// `repeat` where the platform repeats less than the press typed
    /// ([`keymap::text_press_repeat`]).
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
    /// Returns `true` whenever that owner exists, even if its session has closed,
    /// so a stale hold can never inject the bytes into the replacement tab.
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
        // `KeySequence`/`Text` carry no event type, so the repeat is stated here, or a
        // held chord would re-snap the viewport and re-clear the selection every tick.
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

    /// Capture one repeatable GUI/native action after its genuine press was
    /// classified. The normalized action, original window, and any content/
    /// session identity inside it become immutable authority for the hold.
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

    /// One press-disposition tail for [`on_key`]'s gate chain: a gate that
    /// consumed a genuine press either captures a repeatable action
    /// ([`Self::note_local_repeat_press`]) or records a plain consumed press
    /// ([`Self::note_consumed_press`]).
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

    /// Apply one captured local repeat only while its original owner identity
    /// remains live. A stale owner returns `false` and is silently retained until
    /// release; it can never fall through into newly focused content.
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
                // A held key's repeat is typing too: timed as the seam times a
                // press (`input_to_session_inner`'s native boundary).
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

    /// Closes a stale press episode before a fresh non-repeat press takes its physical
    /// key (the OS lost the key-up during a focus epoch): consumed, literal and local
    /// owners retire silently, while a forwarded owner sends its owed release to the
    /// original session so Kitty apps do not see a stuck key.
    fn retire_previous_physical_press_owner(
        &mut self,
        replacement_window: WindowId,
        physical_key: aterm_window::keyboard::PhysicalKey,
    ) {
        if self.physical_press_owners.contains_key(&physical_key) {
            // The ordinary release seam carries every rule.
            self.release_physical_press(replacement_window, physical_key);
        }
    }

    /// Whether `physical_key`'s press is tracked as GUI-consumed: a peek without the
    /// remove ([`take_consumed_release`]) for [`on_key`]'s repeat swallow, since the
    /// release still needs the entry.
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

    /// A repeat without an observed physical press (a focus epoch began mid-hold or
    /// the key-down was lost) has no protocol peer or destination, so it fails closed
    /// rather than fabricating a Kitty repeat or literal payload in whatever is focused.
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

    /// Resolves an OS auto-repeat from press-time physical ownership, at the top of
    /// [`on_key`] before blink, native or shortcut gates, so a content, focus or
    /// modifier change cannot reinterpret a held key. Every repeat is consumed:
    /// encoded and literal owners route to their original session, GUI-consumed owners
    /// stay silent, and an untracked repeat fails closed.
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

    /// Forwards an auto-repeat to the owner established by the physical press, before
    /// live GUI and content gates, so a focus, tab or modifier change cannot redirect
    /// it. The owner stays for later repeats and the release.
    ///
    /// Returns `true` whenever a forwarded owner exists, even if its session just
    /// closed: such a repeat is swallowed, not fabricated in the new session.
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

    /// If `physical_key`'s press was recorded as GUI-consumed ([`note_consumed_press`]),
    /// removes it and returns `true`, so [`on_key`]'s release branch swallows the
    /// release without encoding. Removing on the first release keeps the set
    /// leak-free. `false` when the owner is forwarded or absent.
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

    /// Retire a repeatable GUI/native episode. Local actions never have a Kitty
    /// key-press peer, so release is byte-silent just like a one-shot GUI gate.
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

    /// Finishes one physical hold by its press-time disposition. A release with no
    /// observed press is byte-silent; a forwarded release goes to the original session
    /// with the original logical key, modifiers and layout, except a modifier key's own
    /// bit, which reflects the state the release leaves (as kitty reports it).
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
                // Tolerated key traffic like its press: a release must not bump the
                // activity epoch (falsifying the `exact_activity` Commit fact) or
                // poke a pending seamless-update overlap's cancel channel.
                self.note_update_handoff_tolerated_activity();
                #[cfg(test)]
                let trace_key = key.clone();
                #[cfg(not(test))]
                let _ = window;
                // Byte-silent release: without Kitty `REPORT_EVENT_TYPES`,
                // `encode_key_with_text` encodes nothing for a release, so the seam
                // would take the terminal mutex for nothing. The press published that
                // answer (`publish_release_relevance`), so a proven-silent release skips
                // the seam and reports `Delivery::Full`. `None` asks the engine.
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
                // Keep the seam's per-session ordering: a release seen while a paste
                // drains queues behind it, even though focus moved. A successful enqueue
                // is full delivery. A byte-silent release has nothing to order.
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
    ///
    /// Returns `false` only when no native view is active. Otherwise every press is
    /// consumed once: app/window commands may execute, editor and Settings keys enter
    /// the neutral [`InputEvent`] seam, and terminal-only bindings and raw byte
    /// sequences are swallowed rather than reaching the parked PTY.
    fn on_key_native_mode(&mut self, wid: WindowId, mods: ModifiersState, ev: &KeyEvent) -> bool {
        let Some((_, native_view)) = self.active_native_view(wid) else {
            return false;
        };

        // Configured commands remain useful over app tabs, but an explicit raw
        // sequence is terminal authority and can never cross this boundary. A
        // pane-focus chord with no pane that way is no command here either: it
        // reaches the view's editor below, where Alt+←/→ is a Settings field's
        // word motion (`app_native`'s `alt_word`).
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
                        // Select All over a native view targets the view's own
                        // text model (the TextInput event the menu path sends),
                        // never the parked terminal beneath it.
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

        // Content-agnostic host commands (or fenced no-ops for unsupported native
        // splits), before editor command lowering.
        if HARDCODED_SUPER_CHORDS
            && (self.on_key_super_shift_chord(mods, ev)
                || self.on_key_super_chord(mods, ev)
                || self.on_key_pane_focus(mods, ev))
        {
            self.note_consumed_press(wid, ev);
            return true;
        }

        // Clipboard and find need host capabilities rather than reducer-only key
        // events. Their implementations resolve the active native view and never
        // read the parked terminal here.
        if HARDCODED_SUPER_CHORDS
            && mods.super_key()
            && let Key::Character(character) = &ev.logical_key
        {
            if character.eq_ignore_ascii_case("f") {
                self.find_requested();
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

        // While a platform composition is live, its eventual Ime::Commit owns
        // the text. Direct key lowering would duplicate the composed grapheme.
        if self.native_preedit_active(wid) {
            self.note_consumed_press(wid, ev);
            return true;
        }

        // A native page never emits a Kitty modifier byte, and its reducers
        // ignore Caps/Num Lock, so no IOKit lock-state lookup is made here.
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
        // Platform composition belongs to the window even when a secret field
        // deliberately omits marked text from its native projection. Its final
        // IME commit owns the text; direct key lowering must not insert it too.
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

    /// Resolve only text selected by the active native view. This is the shared
    /// authority for Cmd-C and command-palette enablement; neither may consult the
    /// selection of a terminal hidden beneath a native tab.
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
                // Password fields deliberately expose no plaintext input.
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
            // Key release: only Kitty's REPORT_EVENT_TYPES consumes it, so it skips
            // the press-only handling. A release whose press a GUI gate consumed is not
            // encoded either; the disposition is recorded per physical key at press
            // time, never re-derived from the live gates (modifiers change mid-hold).
            self.release_physical_press(wid, ev.physical_key);
            return;
        }
        // Every newer physical press of a different class is a supersession
        // boundary, including a local action that returns before the PTY seam;
        // releases are not. The press is built once ([`physical_press`]) and
        // classified by [`press_fence`], as `App::input` and the control socket
        // classify it. A key this window turns into a paste ([`Self::press_pastes`])
        // is fenced as that paste.
        let mods = self.windows.get(&wid).map(|ws| ws.mods);
        let option_composes = self.option_composes_in(wid);
        let (ev, mods) = self.option_as_layout_level(ev, mods, option_composes);
        let press = mods.and_then(|m| physical_press(&ev, m, option_composes));
        // Selection custody: a bare modifier/lock press is inert; it does not
        // cancel scroll motion, snap the viewport, or clear the selection.
        // Resolved once here off the key identity; the seam re-derives the same
        // answer from the engine key for presses that reach it.
        let inert = keymap::press_is_inert(&ev);
        // Modifier normalization clones the event's owned Key, so it is done
        // lazily, at most once, for the paste test, the configured bindings and
        // the Cmd-S/R check; never for an ordinary typed press.
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
        // A repeat belongs to the physical press epoch: route it before anything
        // else, or a hold in terminal A could type into terminal B or a native tab.
        if ev.repeat {
            self.route_physical_repeat(wid, ev.physical_key);
            return;
        }
        // No such window ⇒ nothing to do. This snapshot is stale for the press
        // that establishes a modifier (macOS winit queues the bare modifier's
        // `KeyboardInput` before its `ModifiersChanged`), so never gate "is this a
        // modifier press" on it; `keymap::press_is_inert` reads the key identity.
        let Some(mods) = mods else {
            return;
        };
        // Typing restarts the blink period, but a bare modifier is inert, so resting
        // on ⇧ to shift-click does not restart the phase.
        if !inert {
            self.reset_blink(wid);
        }
        // Multi-line-paste confirmation banner (Linux; the native-alert platforms
        // have none): modal over its window, checked before every other gate so
        // nothing fires under a security question. Enter accepts, Escape cancels,
        // everything else is swallowed, so a pastejacking payload cannot get the
        // confirming Return into the shell. [`crate::alert_keys::confirm_key`] is
        // the router the macOS sheet uses. A key for another window passes.
        if self.paste_banner.as_ref().is_some_and(|p| p.wid == wid) {
            let (chars, code) = match &ev.logical_key {
                Key::Named(NamedKey::Enter) => (Some("\r"), 0),
                Key::Named(NamedKey::Escape) => (Some("\u{1b}"), 0),
                _ => (None, 0),
            };
            match crate::alert_keys::confirm_key(true, 0, chars, code) {
                crate::alert_keys::ConfirmKey::Accept => self.answer_paste_banner(true),
                crate::alert_keys::ConfirmKey::Cancel => self.answer_paste_banner(false),
                // Not an answer: swallowed anyway — the banner is modal, and a
                // keystroke reaching the PTY from under it would defeat the guard.
                crate::alert_keys::ConfirmKey::PassThrough => {}
            }
            self.note_press_disposition(wid, &ev, None);
            return;
        }
        // Close/quit confirmation banner (Windows and Linux; `close_banner` is
        // `None` on macOS): while it stands over this window it owns the keyboard:
        // Enter proceeds (the parked gesture replays), Escape cancels, everything
        // else is swallowed. The same [`crate::alert_keys::confirm_key`] router
        // as the paste banner, so a Return under a held modifier still answers.
        // A key aimed at a different window passes untouched.
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
        // An open overlay owns the keyboard: every key is swallowed before any
        // binding, `[key_sequences]` rule or hardcoded chord can fire under it.
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
        // An open tab context menu owns the keyboard as it owns the pointer.
        if self.on_key_tab_menu_mode(wid, &ev) {
            self.note_press_disposition(wid, &ev, None);
            return;
        }
        // An open stats lens (`crate::stats_lens`): the keyboard lens takes its
        // own keys; any other key closes any lens and goes on to the terminal,
        // so the echo frame is the first frame without the lens. The chord Copy
        // is bound to (`ctrl+shift+c` / `ctrl+insert` off macOS) is the lens's
        // copy, as ⌘C is.
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
        // The Menu key (and, under `tab_menu_chord = "on"`, Shift+F10) pops that
        // menu for the focused window's active tab: the OS "context menu for the
        // focused thing" chord, the keyboard route to the popup. Not a
        // rebindable `Action` (an OS convention like Alt+Space; a config typo
        // must not shadow the only keyboard route). `tab_menu_chord`'s `policy`
        // is the escape hatch, a knob that can only surrender keys. Its default
        // claims only the Menu key: Shift+F10 is terminfo `kf22`, which legacy
        // applications bind.
        //
        // Windows and Linux (the in-grid-strip platforms; see the
        // `RightPressPlan::Chrome` arm in `app_mouse`). macOS chips carry a real
        // `NSMenu`.
        //
        // Two things decline the claim: `front_defers_…` hands the key back to a
        // kitty-protocol client that negotiated for it, and
        // `open_active_tab_context_menu` returns `false` for a window with no
        // strip / no terminal tab. Either way the key falls through untouched.
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
        // The inline rename field is asked before the native view and the rebindable
        // chords; a command it does not own settles the edit and falls through.
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
        // Native keyboard ownership: a frontmost native view classifies host
        // commands and lowers the key into the input seam before any terminal path,
        // so a `[key_sequences]` rule cannot write to the parked terminal beneath.
        if self.on_key_native_mode(wid, mods, &ev) {
            return;
        }
        // The Claude Code lights (`crate::claude_lights`): `ctrl+shift+tab` selects
        // the next light, a selected light takes Return/Space/←/→/Escape. Before the
        // rebindable chords, so a binding cannot shadow a selected light's Return.
        if self.on_key_claude_lights(wid, mods, &ev) {
            self.note_press_disposition(wid, &ev, None);
            return;
        }
        // User-rebindable shortcuts (`[keybindings]`, then `[key_sequences]`; see
        // `keybinding::resolve_chord`) take precedence; a miss falls through to the
        // hardcoded matches.
        if !self.keybindings.is_empty() || !self.key_sequences.is_empty() {
            // Match on the modifier-independent base key (`]` under Shift, not
            // `}`) so a binding matches across layouts. For the keypad the
            // encoder keeps the keypad identity (`Numpad5`, `NumpadEnter`) while
            // `base_logical_key` hands this lookup the NumLock-aware main-block
            // key it stands in for (`Key::main_block_twin`), so a binding on
            // `enter` fires for KP_Enter, `1` for a NumLock-on keypad 1, and
            // `end` for it with NumLock off. The keybindings-then-key_sequences
            // precedence is `keybinding::resolve_chord`; running this block
            // before the hardcoded Cmd block (so a key_sequences rule shadows a
            // built-in chord) is `on_key`'s policy.
            //
            // A pane-focus chord on an arrow is decided HERE, before dispatch:
            // with no pane that way it resolves as unbound and this press
            // continues below to the PTY encoder (Alt+← as `ESC[1;3D`, the
            // shell's backward-word), booked as a forwarded press like any other,
            // so its repeats and its release follow it there. Deciding after
            // `dispatch_action` would leave the press noted as consumed below and
            // swallow its release.
            let base = base_key.get_or_insert_with(|| base_logical_key(&ev));
            match keybinding::resolve_chord(
                base,
                ev.physical_key,
                mods,
                &self.keybindings,
                &self.key_sequences,
                |dir| self.focus_pane_would_move(dir),
            ) {
                // A configured chord that would type, copy or move the viewport
                // is suspended while the find field owns input, so a bound
                // `ctrl+a` / `cmd+v` works inside the field and `Paste` cannot
                // write the clipboard to the PTY from under it. Window, tab,
                // pane, font, settings and Find actions still fire.
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
                    // A `[key_sequences]` rule sends raw bytes to the PTY,
                    // overriding the default encoder. Skipped while the find
                    // overlay is open so the keystroke drives the search.
                    let search_active =
                        self.windows.get(&wid).is_some_and(|ws| ws.search.is_some());
                    if !search_active {
                        // A literal sequence's press captures the session and bytes;
                        // its repeats reuse that route (via `route_physical_repeat`)
                        // and its release stays silent.
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
        // Terminal Emacs navigation: native content and configured bindings
        // have had first refusal. A genuine press captures a local
        // repeat owner, so a held Cmd-S/Cmd-R (or, in an agent's composer,
        // Ctrl-S/Ctrl-R) repeats search against the press-time terminal and
        // every Kitty release stays byte-silent; Claude and Codex see zero
        // bytes. At a shell, a REPL or any other program a bare Ctrl-S/R is
        // declined here and reaches the program below as 0x13/0x12 (or its
        // Kitty encoding), booked as a forwarded press so its repeats and
        // release follow it there. Only bare Cmd-S/R (when
        // `HARDCODED_SUPER_CHORDS` is on) and bare Ctrl-S/R qualify, so the
        // base key is normalized only after the modifier gate, and the front
        // program is read only for a bare Ctrl-S/R.
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
        // The hardcoded Cmd/Super suite (chord matchers, pane focus, and the
        // ⌘F/⌘C/⌘V/font-zoom/swallow arms below) is off on Linux: GNOME owns
        // Super, and the seeded `[keybindings]` table carries these commands in
        // Ctrl/Alt spellings. See `HARDCODED_SUPER_CHORDS`.
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
        // Scrollback navigation (xterm / Terminal.app convention): Shift+PageUp
        // / PageDown page through history, Shift+Home / End jump to the oldest
        // line / live bottom, instead of reaching the PTY. Unshifted forms still
        // encode to the app. Runs before snap_to_bottom so scrolling up is not
        // undone. Not while the find field owns input: ⇧Home would desync the
        // view from the highlighted match, and unshifted Home/End are caret
        // motions in the field. Not on the alternate screen either, which has no
        // scrollback to move: the chord goes to the program (less and vim page on
        // Shift+PageUp) instead of doing nothing. (`scrollback_chord` has the
        // Windows spellings.)
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
        // Cmd-F enters find mode; while active, keystrokes drive the find
        // instead of reaching the PTY. (On Linux the seeded Ctrl+Shift+F
        // keybinding is the find chord.)
        if HARDCODED_SUPER_CHORDS
            && mods.super_key()
            && let Key::Character(s) = &ev.logical_key
            && s.eq_ignore_ascii_case("f")
        {
            self.search_enter();
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
        // Cmd-C: copy the selection, without clearing it or moving the viewport.
        // Consumed whether or not the copy succeeds (⌘C is app-reserved). Routed
        // by the press's `wid`, which can differ from `frontmost_window`.
        if HARDCODED_SUPER_CHORDS
            && mods.super_key()
            && let Key::Character(s) = &ev.logical_key
            && s.eq_ignore_ascii_case("c")
        {
            self.copy_selection_in(wid);
            self.note_consumed_press(wid, &ev);
            return;
        }
        // A byte-producing press past this point jumps the viewport back to live; a
        // press that writes nothing keeps the reading position. Keys continuing into
        // the seam take the terminal half there, under its one term lock; of the arms
        // that end a press here only ⌘V and an IME composing key snap for themselves.
        // The window half (glide, bounce, sub-row residual) is cancelled for any
        // non-inert press, so a momentum tail cannot scroll back off the prompt.
        if !inert {
            self.cancel_press_scroll_motion(wid);
        }
        // Cmd-V: paste the system clipboard (bracketed when the app enabled it),
        // keeping the selection. An empty clipboard swallows the key, so it keeps
        // its own snap.
        if hardcoded_paste_chord(mods, &ev) {
            self.snap_to_bottom(wid);
            self.paste_clipboard();
            self.note_consumed_press(wid, &ev);
            return;
        }
        let zoom_repeat = font_zoom_repeat_action(mods, &ev);
        // Font zoom writes no PTY bytes and needs no snap: `Grid::reflow` keeps the
        // pre-resize offset.
        if self.on_key_font_zoom(mods, &ev) {
            self.note_press_disposition(
                wid,
                &ev,
                zoom_repeat.map(crate::LocalRepeatAction::FontZoom),
            );
            return;
        }
        // While a composition is in flight its keystrokes belong to the composer; the
        // text arrives via `Ime::Commit`.
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| keymap::suppress_direct_send(&ws.preedit))
        {
            // A composing key never reaches the seam, so it snaps here.
            self.snap_to_bottom(wid);
            self.note_consumed_press(wid, &ev);
            return;
        }
        // option_as_meta = false: Option (or Alt where winit supplies composed text)
        // types the OS-composed character (Option+a → "å") when it is the sole
        // relevant modifier. Only printable text composes: winit's "\r", "\x08", "\t"
        // and "\x1b" for Enter, Backspace, Tab and Escape go to the encoder, which
        // keeps Option; Space's " " types a space, not `ESC SP`.
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
        // An unclaimed Cmd chord is the application's (macOS terminals never forward
        // Cmd combos; encoding would leak a stray byte or `ESC[1;9D`), swallowed
        // without a snap. Not on Linux, where an unclaimed Super chord is the PTY's.
        if HARDCODED_SUPER_CHORDS && mods.super_key() {
            self.note_consumed_press(wid, &ev);
            return;
        }
        // The rest calls the seam in-thread with an engine-neutral event, so a human
        // key and the `key`/`ctrl` verbs that build the same (Key, mods, base_layout)
        // triple produce byte-identical PTY output. A bare modifier / lock press
        // writes nothing unless the front app negotiated kitty
        // `REPORT_ALL_KEYS_AS_ESC`, so without it the press stops here (decided from
        // the lock-free mode mirror) rather than run the lock-state RPC, terminal
        // mutex and encoder for zero bytes and pollute `input_hot` and the latency
        // histograms; recorded as consumed, its release pairs and is swallowed.
        if inert && !self.front_kitty_reports_all_keys(wid) {
            self.note_consumed_press(wid, &ev);
            return;
        }
        // Map before asking the platform for lock state: an unmapped key cannot reach
        // the wire, so its query is pure latency. A mapped key reads the live state
        // whatever mode is seen here, since the reader can publish Kitty mode before
        // the seam's authoritative encoding.
        if let Some(InputEvent::Key {
            key,
            mods: mapped_mods,
            base_layout,
            layout_text,
            ..
        }) = press
        {
            // Caps/Num Lock are not in winit's `ModifiersState`. Keep `mapped_mods`:
            // Windows' AltGr composition may strip Ctrl+Alt.
            let km_mods = mapped_mods | (self.lock_modifiers)();
            // Always a genuine press: repeats and releases were routed at the top.
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
        // IME/dead-key fallback: no engine key, so a plain layout character types
        // winit's resolved `text` ([`physical_press`] built it bare-only).
        if let Some(text @ InputEvent::Text(_)) = press {
            let repeat = keymap::text_press_repeat(&ev, keymap::modifiers_from_winit(mods))
                .map(InputEvent::Text);
            self.forward_literal_press(wid, ev.physical_key, text, repeat);
            return;
        }
        // No byte-producing press was observed (a dead key, `Unidentified`, a
        // multi-codepoint `Character`, a media key), so a later CSI-u release has no
        // valid peer and is swallowed; nothing was written, so no snap.
        self.note_consumed_press(wid, &ev);
    }

    /// Whether an agent's composer owns the front terminal's prompt, so a bare
    /// Ctrl-S/Ctrl-R is aterm's find there ([`terminal_emacs_search_direction`])
    /// rather than the program's. Prompt ownership, as the citation gate reads
    /// it (`App::resolve_citation`): the agent aterm identifies in front
    /// ([`crate::harness_host::identified_agent`]: by its own name, or a
    /// runtime like `node` by its screen), and only while the group that
    /// publication names is still the PTY's foreground group as a fresh read
    /// finds it. A publication the status sweep has not caught up on speaks for
    /// nobody: the agent exited to its shell, and the shell owns the key.
    /// Nor does one whose agent has handed the terminal to a program it
    /// started in its own group (`SessionFactory::handed_terminal`): Claude
    /// Code runs `$EDITOR` with inherited stdio and no group of its own, so
    /// the group in front and its leader's name stay the agent's while vim
    /// reads the keys, and vim owns the key. Where no foreground group can be
    /// read (a ConPTY on Windows always answers `-1`, and the sweep then names
    /// no group: `SessionTimeline::sweep_slog`), the publication, there the
    /// screen reader's, stands alone. No front terminal (a native tab) ⇒
    /// `false`.
    fn front_agent_composer(&self, wid: WindowId) -> bool {
        let Some(pooled) = self
            .front_terminal(wid)
            .and_then(|terminal| self.pool.get(terminal.session))
        else {
            return false;
        };
        // The timeline is a leaf lock; the kernel is asked after it is let go.
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

    /// Whether the front terminal shows its alternate screen, which keeps no
    /// scrollback, read lock-free from the session's `ModeMirror`. No front
    /// terminal (a native tab) ⇒ `false`.
    fn front_alternate_screen(&self, wid: WindowId) -> bool {
        self.front_terminal(wid)
            .and_then(|t| self.pool.get(t.session))
            .is_some_and(|s| s.ctx.modes.alternate_screen())
    }

    /// Whether the front terminal's keyboard mode carries kitty
    /// `REPORT_ALL_KEYS_AS_ESC`, the one flag under which a bare modifier press
    /// produces bytes. Read lock-free from the session's `ModeMirror`. No front
    /// terminal (a native tab) ⇒ `false`.
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

    /// Hardcoded Cmd-Shift chords of [`on_key`], handled first among the Cmd
    /// combos because they need Shift (which the `!shift_key()` block excludes).
    /// Returns `true` when a chord fired (the caller then returns); `false`
    /// (including an unrecognized Cmd-Shift character) falls through. On a US
    /// layout Shift maps `]`/`[`/`d`/`o`/`m`/`n` to `}`/`{`/`D`/`O`/`M`/`N`, so
    /// both forms are accepted.
    fn on_key_super_shift_chord(&mut self, mods: ModifiersState, ev: &KeyEvent) -> bool {
        if mods.super_key()
            && mods.shift_key()
            && let Key::Character(s) = &ev.logical_key
        {
            match s.as_str() {
                // Cmd-Shift-] / Cmd-Shift-[ cycle tabs (wrapping).
                "]" | "}" => {
                    self.cycle_tab(true);
                    return true;
                }
                "[" | "{" => {
                    self.cycle_tab(false);
                    return true;
                }
                // Cmd-Shift-T reopens a stable descriptor with fresh runtime ids.
                "t" | "T" => {
                    let _ = self.reopen_last_closed_tab();
                    return true;
                }
                // Cmd-Shift-N "Move Tab to New Window": posted as a Wake, since
                // the move's OS attach needs the `ActiveEventLoop`.
                "n" | "N" => {
                    if let Some(proxy) = self.proxy.as_ref() {
                        let _ = proxy.send_event(Wake::DetachActiveTab);
                    }
                    return true;
                }
                // Cmd-Shift-D: split horizontally (panes stacked), the default
                // chord for `Action::SplitHorizontal`.
                "d" | "D" => {
                    self.split_focused_pane(pane::SplitDir::Horizontal);
                    return true;
                }
                // Cmd-Shift-O "Open Active Session in New Window": `on_key` has no
                // `ActiveEventLoop`, so post a Wake for the attach and OS window.
                "o" | "O" => {
                    if let Some(proxy) = self.proxy.as_ref() {
                        let _ = proxy.send_event(Wake::ViewActiveSessionInNewWindow);
                    }
                    return true;
                }
                // Cmd-Shift-M "Move Tab to Next Window" (wrapping); no OS-window
                // attach, so the move runs directly.
                "m" | "M" => {
                    let _ = self.migrate_active_tab_to_next_window();
                    return true;
                }
                // Cmd-Shift-L: the ledger key (`Action::OpenLedger`'s default).
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

    /// Hardcoded Cmd (no Shift) chords of [`on_key`]: N new window, T new tab, D split,
    /// W close the focused pane, Q the quit fallback, 1-9 switch tab. Returns `true`
    /// when a chord fired.
    fn on_key_super_chord(&mut self, mods: ModifiersState, ev: &KeyEvent) -> bool {
        if mods.super_key()
            && !mods.shift_key()
            && let Key::Character(s) = &ev.logical_key
        {
            let lc = s.to_ascii_lowercase();
            match lc.as_str() {
                "n" => {
                    // Booked in this turn like Cmd-T's tab (`tab_spawn`), so the keys
                    // after it wait for the new window.
                    self.open_new_window();
                    return true;
                }
                "t" => {
                    self.open_tab();
                    return true;
                }
                // Cmd-D: split the focused pane vertically (panes side by side).
                "d" => {
                    self.split_focused_pane(pane::SplitDir::Vertical);
                    return true;
                }
                // Cmd-Q fallback for when AppKit declines the menu's key equivalent
                // (menu not installed, item disabled): the same Wake, the same
                // confirm. A held Cmd-Q's repeats replay the consumed disposition.
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
                // Cmd-1..Cmd-9 → switch to that tab (1-based → 0-based index).
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

    /// Pane chords (iTerm2 parity): Cmd-Opt-Arrow focuses a neighbour pane,
    /// Cmd-Shift-Enter toggles zoom. Returns `true` when one fired.
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
        // Cmd-Opt-Arrow: directional focus.
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

    /// Classify a find-mode keystroke into the repeatable action it drives: a match
    /// step/repeat (`↑`/`↓`, `^S`/`^R`, `⌘S`/`⌘R`), or one field edit from the shared
    /// keymap ([`field_edit_action`]), each repeating on a held key.
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
        // Match navigation gets first claim, then the generic field keymap. The
        // two sets are disjoint (↑/↓, F3 and ^S/^R/⌘S/⌘R are not field motions).
        match &ev.logical_key {
            Key::Named(NamedKey::ArrowDown) => return Some(Action::Step(true)),
            Key::Named(NamedKey::ArrowUp) => return Some(Action::Step(false)),
            // F3 / Shift+F3: find next / previous, the Windows (and VS Code) reflex
            // `find_bar::hint_text` teaches there. Only while the field is open: a
            // closed find leaves F3 to the program (`keybinding`'s seeding note).
            #[cfg(not(target_os = "macos"))]
            Key::Named(NamedKey::F3)
                if !mods.control_key() && !mods.alt_key() && !mods.super_key() =>
            {
                return Some(Action::Repeat(!mods.shift_key()));
            }
            // emacs isearch-repeat.
            _ if (ctrl || bare_cmd) && base_is("s") => return Some(Action::Repeat(true)),
            _ if (ctrl || bare_cmd) && base_is("r") => return Some(Action::Repeat(false)),
            _ => {}
        }
        field_edit_action(mods, ev).map(Action::Edit)
    }

    /// Classify a keystroke aimed at the in-grid rename field into its repeatable
    /// edit, paired with the edit's session so a held ⌫ repeats against the target
    /// captured at press time.
    fn rename_repeat_action(
        &self,
        wid: WindowId,
        mods: ModifiersState,
        ev: &KeyEvent,
    ) -> Option<(u64, crate::app_search::SearchEdit)> {
        let session = self.inline_rename_edit(wid)?.session;
        Some((session, field_edit_action(mods, ev)?))
    }

    /// Open a directed terminal search or repeat an already-open one, then bind the
    /// complete physical hold to that local action. Repeats never re-run live shortcut
    /// routing, and release never reaches the Kitty encoder.
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

    /// A live IME composition owns its keystrokes; its result arrives once, as
    /// `Ime::Commit`.
    fn grid_ime_composing(&self, wid: WindowId) -> bool {
        self.windows
            .get(&wid)
            .is_some_and(|ws| !ws.preedit.is_empty())
    }

    /// In-grid rename dispatch of [`on_key`]: while the strip's inline pin editor owns
    /// `wid`, a keystroke drives the field, with the policy of
    /// [`Self::divert_menu_action_around_rename`]: field edits, paste, ⎋ and ⏎/⇥ are
    /// the editor's (`true`); a bare modifier is swallowed (`true`); anything else
    /// settles the edit and returns `false`, so its command runs against a settled
    /// world. Above the `[keybindings]` / `[key_sequences]` block, so ⎋ never reaches
    /// the PTY and a `[key_sequences]` chord's bytes go out only after the field
    /// closed. A native (macOS) edit belongs to AppKit's field editor.
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
        // Whether this keystroke is the chord Paste is bound to. Resolved before
        // the match because a guard may not borrow `self` that the arm needs
        // mutably. `[keybindings]` alone is the right map: it wins over
        // `[key_sequences]` in `resolve_chord`, and asking it directly avoids
        // `resolve_chord`'s Vec.
        let paste_chord = self
            .keybindings
            .action_for(&base, ev.physical_key, mods)
            .is_some_and(keybinding::Action::is_paste);
        match &ev.logical_key {
            // ⎋ is the one cancel, exactly as in the macOS field editor.
            Key::Named(NamedKey::Escape) => {
                self.cancel_session_rename(wid, session);
                return true;
            }
            // ⏎ and ⇥ commit. Empty clears the pin, falling back to the ladder the
            // placeholder shows.
            Key::Named(NamedKey::Enter | NamedKey::Tab) => {
                let text = self
                    .inline_rename_edit(wid)
                    .map(|edit| edit.text.clone())
                    .unwrap_or_default();
                self.commit_session_rename(wid, session, &text);
                return true;
            }
            // ⌘V pastes into the field, never the PTY behind it.
            _ if bare_cmd && matches!(&base, Key::Character(c) if c.eq_ignore_ascii_case("v")) => {
                self.rename_paste_in(wid);
                return true;
            }
            // …and so does the configured paste chord, the only one off macOS:
            // the Windows/Linux defaults (`ctrl+v`, `ctrl+shift+v`,
            // `shift+insert`, `Keybindings::platform_defaults`) reach a paste
            // action through the keybinding map, and the field pastes the
            // clipboard for either. Falling to `_ => {}` below would
            // settle the edit and return `false`, and `on_key`'s keybinding
            // block below this gate would then write the clipboard to the PTY.
            // Resolving against the same map that block consults keeps the two
            // routes agreeing: whatever chord Paste is bound to, the field
            // claims.
            //
            // `find_suspends_action` is not the place for this: it suspends an
            // action so the find field owns the keystroke, while the rename
            // field settles and falls through for everything it does not own.
            _ if paste_chord => {
                self.rename_paste_in(wid);
                return true;
            }
            _ => {}
        }
        // Anything else is the user leaving the field to do something else.
        self.settle_rename_edit(wid);
        false
    }

    /// The engine-neutral twin of [`Self::on_key_rename_mode`] for controller
    /// `key`/`text`/`paste` verbs, so a headless editor can always be ended. Returns
    /// whether the field consumed the event.
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
        // The keypad is its main-block twin here: `keymap::build_key_input` keeps
        // the keypad identity (`Numpad5`, `NumpadEnd`) for the PTY encoders, and
        // `InputEvent::keypad_folded` is the one fold, shared with the native
        // pages.
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

    /// Find-mode keystroke dispatch of [`on_key`]: while a window's `search` is
    /// active, keystrokes drive the find (query edit + match navigation) instead
    /// of reaching the PTY. Returns `true` whenever search is active (the caller
    /// then returns); `false` when no search is in flight.
    ///
    /// The keymap is emacs-isearch shaped: `⌘S`/`⌘R` and `^S`/`^R` step to the
    /// next/previous match (on an empty query they recall the last accepted
    /// search, the `C-s C-s` idiom), `⏎` accepts (exit at the match, highlight
    /// kept), `⎋`/`^G` cancel (exit, restoring the pre-find viewport), and
    /// `↓`/`↑` mirror `^S`/`^R`. The case/regex toggles are `⌥⌘C`/`⌥⌘R`, not
    /// plain ⌥ chords, which must stay free for Option-composed characters (é,
    /// ß, …) on macOS. Off macOS they also answer to plain `Alt+C`/`Alt+R` (the
    /// VS Code find-widget chords): Alt composes nothing there, and ⌥⌘ becomes
    /// Win+Alt (Win+Alt+R is the Xbox Game Bar's).
    ///
    /// Everything else is text-field editing, classified (and made
    /// key-repeatable) by [`Self::search_repeat_action`]: caret motion,
    /// word/line kills, and printable insertion. `⌘V` pastes into the field
    /// (never the PTY), as does any chord bound to a paste action, which the
    /// suspension arm in `on_key` routes via [`Self::search_paste_in`] before
    /// this handler runs. ⎋ and ⏎ are commands, so the classifier's text arm
    /// filters control characters.
    fn on_key_search_mode(&mut self, wid: WindowId, mods: ModifiersState, ev: &KeyEvent) -> bool {
        if self.windows.get(&wid).is_none_or(|ws| ws.search.is_none()) {
            return false;
        }
        if self.grid_ime_composing(wid) {
            return true;
        }
        // Chords match on the modifier-independent base key: macOS ⌥ composes a
        // different glyph into `logical_key` (⌥⌘C arrives as "ç"), and a
        // non-Latin layout's letter reads as its US key (`field_chord_key`).
        let base = field_chord_key(ev, mods);
        let base_is =
            |want: &str| matches!(&base, Key::Character(c) if c.eq_ignore_ascii_case(want));
        let ctrl = mods.control_key() && !mods.super_key() && !mods.alt_key();
        let cmd_alt = mods.super_key() && mods.alt_key() && !mods.control_key();
        let bare_cmd =
            mods.super_key() && !mods.shift_key() && !mods.control_key() && !mods.alt_key();
        // ⌘C falls through to the terminal's copy, which copies the selected match
        // without closing find.
        if bare_cmd && base_is("c") {
            return false;
        }
        // Alt+C / Alt+R off macOS, before the field classifier so a layout whose
        // Alt+letter yields `ev.text` toggles rather than types. AltGr is safe: winit
        // strips its synthetic Ctrl+Alt, and LeftCtrl+LeftAlt fails `!control_key`.
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
            // ⏎ accepts (emacs RET): leave find mode, staying at the current match.
            Key::Named(NamedKey::Enter) => self.search_accept_in(wid),
            // ^G: the emacs abort chord — cancel, restoring the pre-find viewport.
            _ if ctrl && base_is("g") => self.search_cancel_in(wid),
            // ⌘V pastes into the field (one line, control bytes stripped).
            _ if bare_cmd && base_is("v") => self.search_paste_in(wid),
            // ⌥⌘C / ⌥⌘R: match-case / regex toggles (app-sticky for the next find).
            _ if cmd_alt && base_is("c") => self.search_toggle_case_in(wid),
            _ if cmd_alt && base_is("r") => self.search_toggle_regex_in(wid),
            _ => {}
        }
        true
    }

    /// The window half of the press-path viewport snap: cancel any in-flight
    /// wheel glide, elastic-overscroll bounce, and banked sub-row residual, so
    /// a momentum tail cannot ease the viewport off the prompt after a key.
    /// Split from [`App::snap_to_bottom`] because the terminal half
    /// (`display_offset` → bottom) is done in the seam's term-lock scope for
    /// every key that reaches it, and this half needs no lock. Arms that end a
    /// press before the seam still call the full `snap_to_bottom`.
    fn cancel_press_scroll_motion(&mut self, wid: WindowId) {
        self.drop_front_scroll_motion(wid);
    }

    /// Drop the window's wheel glide, its sub-row residual and its bounce when
    /// they are the front pane's. A glide the wheel started on another, unfocused
    /// pane keeps running: a key typed into the focused pane, its snap to live or a
    /// `ScrollView` jump there must not lose the other pane's scroll.
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

    /// What a held key repeats against the open palette: its press's own
    /// [`crate::app_palette::palette_key`] classification, so a held `⌫`/`^W` keeps
    /// deleting and a held `↓` keeps moving. Close, activate and a swallowed chord do
    /// not repeat. `None` when no palette is open on `wid`.
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

    /// The modal-overlay key gate: if an overlay is open on `wid`, delegate to its
    /// handler and swallow the key (`true`); closed ⇒ `false`. A ⌘ chord is swallowed
    /// without reaching the overlay: a menu row the bar greys hands its key
    /// equivalent to `on_key`, and the palette would otherwise type it into
    /// its search (⌘C as `c`).
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

    /// While a tab context menu is open on `wid`, drive it from the keyboard and
    /// swallow every key (`true`); closed ⇒ `false`. The Win32 menu bindings, since a
    /// self-drawn popup gets none for free: ↑/↓ step over enabled rows, wrapping;
    /// Home/End jump to the first/last; ↵ / Space activate; Esc dismisses; a bare
    /// modifier or lock is inert; any other key dismisses and is swallowed, so it
    /// cannot land in the shell behind the menu. The key table is
    /// [`crate::tab_menu::nav_for_key`], shared with [`Self::tab_menu_input_event`];
    /// this winit adapter adds the bare-modifier class (`keymap::press_is_inert`,
    /// which also covers AltGr / Fn / Symbol).
    fn on_key_tab_menu_mode(&mut self, wid: WindowId, ev: &KeyEvent) -> bool {
        if !self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.tab_menu.is_some())
        {
            return false;
        }
        // A bare modifier does not dismiss the card, so the ⇧ of ⇧F10 cannot close
        // the menu the chord opens.
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

    /// Apply one [`MenuNav`](crate::tab_menu::MenuNav) to `wid`'s open card: the sole
    /// mutator behind both routes. Returns whether a menu was open.
    fn tab_menu_nav(&mut self, wid: WindowId, nav: crate::tab_menu::MenuNav) -> bool {
        use crate::tab_menu::MenuNav;
        let Some((entries, highlight, limit)) = self.windows.get(&wid).and_then(|ws| {
            let menu = ws.tab_menu.as_ref()?;
            // No painted rect yet (the card popped this turn and the frame has
            // not landed) ⇒ the whole model is a fair bound; the paint clamps.
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
                // ↵ with nothing lit is a dismiss, not a guess.
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
            // Swallowed and otherwise ignored — the card is untouched.
            MenuNav::Inert => {}
        }
        true
    }

    /// Whether the front terminal's application negotiated the keyboard enhancement
    /// that makes `key` reportable, and so keeps it: a host gesture may only claim a
    /// key the application cannot otherwise receive. The Menu key (kitty `57363`)
    /// reaches an app under `DISAMBIGUATE_ESC_CODES` or `REPORT_ALL_KEYS_AS_ESC` and
    /// encodes to nothing otherwise; Shift+F10 is `ESC[21;2~` (`kf22`) even in legacy,
    /// so only `REPORT_ALL_KEYS_AS_ESC` buys it back. Never calls an encoder.
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

    /// The convergence-seam twin of [`Self::on_key_tab_menu_mode`] plus the
    /// `on_key` chord arm: the tab context menu driven by an engine-neutral
    /// [`InputEvent`](crate::input::InputEvent). Returns `true` when the menu
    /// consumes the event. `aterm ctl key menu` / `ctl key shift+f10` arrive here
    /// via `Wake::Input`, never through `on_key`, so both routes must answer alike.
    ///
    /// A controller press carries a grid cell, not the pixels the card's hit-test
    /// takes, so it dismisses and is consumed; on both routes no pointer event reaches
    /// the PTY or the viewport while the card is up.
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
                // Text, raw bytes and a paste are unhandled keys: dismiss and swallow.
                InputEvent::Text(_) | InputEvent::KeySequence(_) | InputEvent::Paste(..) => {
                    self.close_tab_menu(wid);
                    true
                }
                // A controller press cannot be hit-tested against the card, so it
                // dismisses and is consumed, with its release and motion.
                InputEvent::MouseButton {
                    button,
                    pressed: true,
                    ..
                } => {
                    self.close_tab_menu(wid);
                    // Latch the button so the closed path swallows its release.
                    if let Some(ws) = self.windows.get_mut(&wid) {
                        ws.tab_menu_consumed_button = Some(*button);
                    }
                    true
                }
                // Every other pointer event, the wheel and view scrolls are swallowed.
                InputEvent::MouseButton { .. }
                | InputEvent::MouseMove { .. }
                | InputEvent::Wheel { .. }
                | InputEvent::ScrollView(_) => true,
                _ => false,
            };
        }
        // Closed, but the press that dismissed the card may still be down: swallow
        // its release and drag motion, so a tracking TUI never gets half a pair.
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

        // Closed: the chord may pop it, on the in-grid-strip platforms (Windows
        // and Linux) and only for a genuine press (a repeat of a held Menu key
        // must not re-pop the card). A control client's chord under a hold is
        // swallowed with no card, as its `open tab-menu` is refused.
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

    /// Cmd-= / Cmd-+ / Cmd-- / Cmd-0 live font zoom (grow / shrink / reset) of
    /// [`on_key`]. Returns `true` when a zoom chord fired.
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

    /// Run a user-bound [`keybinding::Action`]: each arm calls the method the built-in
    /// key calls, routed to the originating window `wid`.
    pub(crate) fn dispatch_action(&mut self, wid: WindowId, action: keybinding::Action) {
        use keybinding::Action;
        // A consumed keybinding action is app-level intent and revokes a pending
        // update overlap (it changes topology or presentation the carried frame
        // would contradict); plain typing goes through `input_to_session`'s policy.
        self.note_update_handoff_activity();
        match action {
            Action::NewTab => self.open_tab(),
            Action::ReopenClosedTab => {
                let _ = self.reopen_last_closed_tab();
            }
            Action::CloseTab => {
                // `pending_close` goes on the window whose last tab closed (the
                // frontmost, which `close_active_tab` operated on), not `wid`.
                let _ = wid;
                if let Some(closed) = self.close_active_tab()
                    && let Some(ws) = self.windows.get_mut(&closed)
                {
                    ws.pending_close = true;
                }
            }
            // Booked in this turn, as Cmd-N is.
            Action::NewWindow => self.open_new_window(),
            Action::NextTab => self.cycle_tab(true),
            Action::PrevTab => self.cycle_tab(false),
            // 1-based as the user wrote it → 0-based index (Cmd-1..Cmd-9 parity).
            Action::SwitchTab(n) => self.switch_tab(usize::from(n).saturating_sub(1)),
            Action::SplitVertical => self.split_focused_pane(pane::SplitDir::Vertical),
            Action::SplitHorizontal => self.split_focused_pane(pane::SplitDir::Horizontal),
            Action::FocusPaneLeft => self.focus_pane(pane::FocusDir::Left),
            Action::FocusPaneRight => self.focus_pane(pane::FocusDir::Right),
            Action::FocusPaneUp => self.focus_pane(pane::FocusDir::Up),
            Action::FocusPaneDown => self.focus_pane(pane::FocusDir::Down),
            Action::TogglePaneZoom => self.toggle_pane_zoom(),
            // Window-routed: `wid` is the window the chord was pressed in (off macOS
            // the seeded `ctrl+shift+c` / `ctrl+insert` are the primary copy chords).
            Action::Copy => {
                self.copy_selection_in(wid);
            }
            Action::Paste => {
                // A paste, like the hardcoded Cmd-V, jumps the viewport to live.
                self.snap_to_bottom(wid);
                self.paste_clipboard();
            }
            // PRIMARY, the middle click's paste, through the same guarded
            // pipeline; the clipboard where the platform has no PRIMARY.
            Action::PasteSelection => {
                self.snap_to_bottom(wid);
                #[cfg(target_os = "linux")]
                self.paste_primary_into(wid);
                #[cfg(not(target_os = "linux"))]
                self.paste_clipboard();
            }
            Action::Find => self.find_requested(),
            Action::FontIncrease => self.set_font_size(self.font_size + FONT_ZOOM_STEP),
            Action::FontDecrease => self.set_font_size(self.font_size - FONT_ZOOM_STEP),
            Action::FontReset => self.set_font_size(self.default_font_size),
            // Scrollback viewport navigation (the SAME machinery the wheel/trackpad
            // and the control socket drive via InputEvent::ScrollView).
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
            // Windowed, relay through the menu wake so every platform converges on the
            // Settings-tab opener; headless tests have no proxy.
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
            // Per-session: flips the front session of the window the chord landed
            // in.
            Action::ToggleMatrixRain => self.toggle_matrix_rain(wid),
            Action::ToggleSeriousMode => {
                self.user_toggle_serious_mode();
            }
            Action::OpenPalette => self.toggle_palette(),
            Action::ChooseTheme => self.open_theme_palette(),
            // Opens the inline pin editor over the ACTIVE tab of the window the
            // chord landed in — the keyboard face of the double-click.
            Action::RenameSession => {
                self.begin_active_session_rename(wid);
            }
            // The `MenuAction::FindNext` reducer; a key path has no refusal surface.
            Action::FindNext => {
                let _ = self.search_find_again(true);
            }
            Action::FindPrev => {
                let _ = self.search_find_again(false);
            }
            // The ledger key: the same method the hardcoded ⇧⌘L calls.
            Action::OpenLedger => self.open_session_ledger(wid),
            // Inert under an open find, as the menu's SelectAll arm: the find bar
            // borrows the terminal selection for its highlight. (`on_key` parks
            // this earlier via `find_suspends_action`; this covers other routes.)
            Action::SelectAll => {
                if self.find_field_window().is_none() {
                    self.select_all();
                }
            }
            // The winit borderless-fullscreen toggle the View menu row fires
            // (F11 is seeded off macOS).
            Action::ToggleFullscreen => {
                let _ = self.toggle_fullscreen();
            }
        }
    }

    /// A composition update (`Ime::Preedit`): track the marked text and the caret's
    /// start within it, so the composition can render and direct key sends stay
    /// suppressed; an empty preedit ends it. The paint is routed at render time by
    /// [`Self::preedit_owner`]. Native text fields get the full byte range, since
    /// they paint both selection and caret.
    pub(crate) fn on_ime_preedit(
        &mut self,
        wid: WindowId,
        text: String,
        cursor: Option<(usize, usize)>,
    ) {
        // A preedit sends no bytes, so it licenses nothing; close the older licence
        // before native/modal handling can return.
        self.clear_move_license(wid);
        // Track the composition on the window before any native-tab routing:
        // `ws.preedit` feeds `on_key`'s suppress_direct_send gate, and left
        // stale it would swallow every later terminal key press.
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
        // A person composing (CJK, dead keys) is a person typing, though the
        // preedit sends no bytes and moves no screen generation; stamp it so a
        // supervisor does not act mid-composition. The commit reaches the
        // session through the input seam as text, which stamps it there.
        if changed
            && !text.is_empty()
            && let Some(session) = self.focused_session_id(wid)
            && let Some(owner) = self.pool.get(session)
        {
            // The preedit reaches no program: it can dismiss no box. An owed log
            // wake rang the session's watchers in the mark's hold.
            let _ = note_person_reaching(&owner.ctx, crate::human_input::Gesture::Other, false);
        }
        if changed && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
    }

    /// The platform turned text input on or off (`Ime::Enabled` / `Ime::Disabled`):
    /// end any composition, so the suppression gate cannot stick, and forget the
    /// `set_ime_cursor_area` memo. On Wayland `Ime::Enabled` follows a
    /// `zwp_text_input_v3.enter` whose `enable()` resets the compositor's cursor
    /// rectangle, which winit does not re-send; the dropped memo and the redraw make
    /// the next frame re-send it (even with no composition in flight), or ibus/fcitx
    /// park the candidate list at the window origin.
    pub(crate) fn on_ime_enablement_changed(&mut self, wid: WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.last_ime_rect = None;
        }
        // Drop any composition the transition invalidated — same stale-preedit
        // hazard `on_ime_preedit`'s window-first write exists for.
        self.on_ime_preedit(wid, String::new(), None);
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
    }

    /// Tell the platform where the caret is (winit `set_ime_cursor_area`) so the
    /// candidate window appears there rather than at the window origin. Calls winit
    /// only when the pixel rectangle changes; a hidden caret is left untouched. `cur`
    /// is the focused pane's cursor in pane coords and `off` its window-space origin.
    /// During a composition the caret is the composition's start plus everything
    /// composed (`aterm_core::render::ime_preedit_caret_col`), and `vis` is forced
    /// true whether or not DECTCEM shows the cursor.
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
        // Terminal-band cell → frame cell: the chrome rows (strip + status bars)
        // sit above the band, so the frame row is the band row plus the chrome
        // height (the same shift the prepends apply to the cells themselves).
        let frame_cell = (
            (cur.0.saturating_add(off.0)) as usize + usize::from(self.chrome_rows(wid)),
            (cur.1.saturating_add(off.1)) as usize,
        );
        self.report_ime_cursor_area_frame(wid, frame_cell);
    }

    /// [`Self::report_ime_cursor_area`]'s core, in frame cells (row 0 is the top strip
    /// row when a strip is on), so a caret inside the strip (the rename well,
    /// [`PreeditOwner::Rename`]) can anchor the candidate window on its own cell.
    pub(crate) fn report_ime_cursor_area_frame(
        &mut self,
        wid: WindowId,
        frame_cell: (usize, usize),
    ) {
        // This window's own metrics (mixed-DPI), read before the `&mut
        // self.windows` borrow below.
        let (cw, ch) = self.win_cell_size(wid);
        let pad = self.win_pad(wid);
        // The inverse of `app_render::pixel_to_term_cell`: Y uses the top pad plus
        // the chrome headroom (`pad_top + head`); X carries no band and stays on
        // `pad`.
        let pad_top = self.win_pad_top(wid);
        let head = self.win_head(wid);
        // The frame origin behind the leading remainder bands (the inverse of
        // `window_to_frame`), `(0, 0)` at exact grid fit.
        let (band_x, band_y) = self.frame_origin(wid);
        // Resolved before consulting the memo, so same-cell DPI/padding/remainder
        // changes are not suppressed as duplicates.
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

    /// Window-space physical caret box shared by terminal cells and native
    /// text fields. Geometry is resolved before this one OS-report memo.
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

    /// Composition committed (`Ime::Commit`): every host's input-method commit (XIM,
    /// Wayland text-input, Windows IMM, macOS `insertText:` including dictation).
    /// Ends the composition and sends the text as typed text, each grapheme encoded
    /// as a `Character` key. A commit holding a control character beside other text
    /// is a paste ([`Self::deliver_paste`]; see [`keymap::committed_text_is_paste`]);
    /// a lone C0 or DEL is the key X11 commits under an open XIM preedit and stays
    /// typed.
    pub(crate) fn on_ime_commit(&mut self, wid: WindowId, text: String) {
        // One routing decision, so the fence and the consumer agree.
        let route = self.ime_route(wid);
        // A grid commit holding a control character is a paste (L2), unless it
        // is one C0 or DEL key (`committed_text_is_paste`). Not while an
        // overlay or the tab menu owns the window: the seam hands the commit to
        // them as text and nothing reaches the PTY, whereas the paste path pins
        // the session behind them.
        let as_paste = keymap::committed_text_is_paste(&text) && self.typing_reaches_grid(wid);
        // This fence precedes every consumer below (native tabs, rename/find
        // fields, the empty commit). A commit that reaches the PTY is more
        // typing, not a class change: it stamps its own typed licence in
        // `App::input` below, and the banked stamps of keys still in flight
        // ahead of it must survive, so it takes the bank-preserving supersede
        // the Text arm applies. A commit a native view, the rename field or the
        // find bar consumes takes the full fence, and one that pastes takes a
        // paste press's fence ([`press_fence`]), at the gesture as ⌘V does,
        // since the guard can hold the delivery.
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
        // End the composition on the window even when a native tab consumes the
        // committed text (the stale-preedit hazard of `on_ime_preedit`).
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.preedit.clear();
            ws.preedit_caret = None;
            // The composition clock closes wherever the text lands; an empty commit
            // is a cancelled composition.
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
            // The rename field claims composed text ahead of find.
            PreeditOwner::Rename => {
                self.rename_field_edit(wid, crate::app_search::SearchEdit::Insert(text));
            }
            // Find owns composed text too (macOS ⌥-dead-keys arrive as a commit).
            PreeditOwner::Find => {
                self.search_edit_in(wid, crate::app_search::SearchEdit::Insert(text));
                if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                    w.request_redraw();
                }
            }
            // The path ⌘V takes: snap at the gesture, the paste pinned to this
            // terminal.
            PreeditOwner::Grid if as_paste => {
                self.snap_to_bottom(wid);
                let session = self.focused_session_id(wid);
                self.deliver_paste(wid, session, text, Source::Human);
                if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                    w.request_redraw();
                }
            }
            // The seam's Text path, as the controller's text egress; a gone window
            // reaches the seam's own refusal.
            PreeditOwner::Grid | PreeditOwner::None => {
                self.input(wid, InputEvent::Text(text), Source::Human);
                if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                    w.request_redraw();
                }
            }
        }
    }

    /// Whether window `wid`'s typing reaches its terminal grid: composed text would commit to
    /// the grid ([`Self::ime_route`]: no native page, inline rename or find bar takes it), and
    /// no overlay (the palette, a card, the session picker) or tab menu takes the keys first.
    /// `false` for a window that is gone.
    pub(crate) fn typing_reaches_grid(&self, wid: WindowId) -> bool {
        matches!(self.ime_route(wid), PreeditOwner::Grid)
            && self
                .windows
                .get(&wid)
                .is_some_and(|ws| !ws.overlay_open() && ws.tab_menu.is_none())
    }

    /// Whether window `wid`'s Option keys should be an Alt ([`crate::platform::set_option_as_alt`]):
    /// in Meta mode (`option_as_meta`, the default) while the window's typing reaches its
    /// terminal ([`Self::typing_reaches_grid`]), whose encoder sends an Option chord as ESC
    /// and the key, or a native Editor ([`Self::typing_reaches_editor`]), whose keymap reads
    /// an Option chord as its M- chord (Option+b is M-b, backward-word; Option+u is M-u,
    /// never a dead key). A GUI field that owns the keyboard (the find bar, the inline
    /// rename, the palette, another native page) keeps Option composing, so German Option+L
    /// types '@' there and Option+e e types 'é' (ATERM-KI-1015-9).
    pub(crate) fn option_is_alt(&self, wid: WindowId) -> bool {
        self.option_as_meta && (self.typing_reaches_grid(wid) || self.typing_reaches_editor(wid))
    }

    /// Whether window `wid`'s typing reaches a native Editor (amacs, design §4.4): the front
    /// view is an Editor buffer, and no overlay, tab menu or inline rename takes the keys
    /// first. Settings, a Markdown page and the recovery page are not editors: their fields
    /// type what Option composes. `false` for a window that is gone.
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

    /// Bring every window's Option keys to [`Self::option_is_alt`]: the one writer of the
    /// setting after a window's creation, run as every event-loop callback ends
    /// (`window_event`, `user_event`, `about_to_wait`), so whatever an event changed (a field
    /// opened or closed, a native page came to the front, a config reload flipped
    /// `option_as_meta`) is in force for the next key event AppKit reads. The OS window is
    /// told only when its setting changes; a window without one (headless) keeps the record.
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

    /// An Option chord's press as Meta mode sends it ([`Self::on_key`]): while the window
    /// reads Option as an Alt (`option_composes` false) an Option chord goes out as ESC and
    /// the key, except one whose character its layout reaches ONLY with Option
    /// ([`crate::platform::OptionOnly`]: German Option+L `@`, Option+7 `|`, Option+N `~`;
    /// French Option+( `{`; British Option+3 `#`). Sending that one as Meta would leave the
    /// character out of reach in every tab, so there Option is the layout's own level, as
    /// AltGr is on Linux: the press types the character, with Option taken off the
    /// modifiers it carries. Every Option chord of the U.S. layout stays Meta (its Option
    /// characters are not ASCII, or a key of their own types them too), and a chord with
    /// Control or Command is left as it came (ATERM-KI-1015-9).
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
    /// ([`crate::platform::option_composes`] of the setting the window was last given), for
    /// `keymap::build_key_input`.
    fn option_composes_in(&self, wid: WindowId) -> bool {
        crate::platform::option_composes(
            self.windows
                .get(&wid)
                .is_some_and(|ws| ws.option_as_alt.get()),
        )
    }

    /// Where composed text on window `wid` lands: the one routing precedence
    /// [`Self::on_ime_commit`]'s dispatch and [`Self::preedit_owner`]'s paint
    /// both read (a frontmost native view, then the inline rename field, then
    /// the open find field, then the grid). Says nothing about whether a
    /// composition is in flight (`preedit_owner` adds that gate);
    /// [`PreeditOwner::None`] only when the window is gone.
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

    /// Which surface owns the in-flight IME composition on window `wid`, through
    /// the same [`Self::ime_route`] as [`Self::on_ime_commit`], so the composition
    /// paints where its commit would land: the redraw path picks the splice that
    /// paints the preedit and anchors the candidate window. With no composition in
    /// flight it returns [`PreeditOwner::None`] after one `is_empty()` check.
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

    /// Current keyboard modifiers as a mouse-report modifier mask (shift/alt/ctrl
    /// bits the engine ORs into the button byte).
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

    /// `invoke <action>`: fire a menu action by name ([`Self::dispatch_menu_action`]),
    /// gated by the live palette row's `enabled`, so a disabled row is refused with a
    /// reason. Names are the `action=` tokens `controls menu` prints.
    pub(crate) fn invoke_menu_action_by_name(
        &mut self,
        el: &ActiveEventLoop,
        name: &str,
    ) -> Result<String, String> {
        let mut p = crate::palette::PaletteState::new();
        p.resolve(&self.palette_live());
        let action = p.action_by_name(name)?;
        let question_stood = self.close_banner.is_some();
        // Clear first: a refusal left by an earlier invocation must never be
        // reported against this one.
        self.pending_action_refusal = None;
        self.dispatch_menu_action(el, action);
        self.invoke_reply(name, question_stood)
    }

    /// `invoke`'s reply once its action has run; `question_stood` is whether a
    /// close/quit question was already up. A declined arm is `ERR`, since `OK` would
    /// tell a driver the action happened. A close or quit that asks first (Windows,
    /// `close_confirm`) is `OK` in words that cannot be read as a close done
    /// (`confirm pending …`); one refused because another question stood is `ERR`.
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

    /// Hand one picker-approved path to the capability-bounded document opener, which
    /// canonicalizes and validates it and mints the grant the Markdown/editor runtime
    /// consumes. With an event loop the read runs on a worker and the tab lands via
    /// `Wake::DocumentAdmitted`, so the returned line only says admission started;
    /// headless, admission is inline and the line is the installed tab's.
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
        // A headless process has no application object to present the panel from, so
        // the modal would never return.
        if self.headless {
            crate::logging::stderr_line!(
                "aterm-gui: {title} needs a window; a headless instance has no file picker"
            );
            return;
        }
        let Some(path) = menu::choose_local_file(title, prompt) else {
            return;
        };
        // An error here is a request-shape problem; a read failure is reported by the
        // `Wake::DocumentAdmitted` arm.
        if let Err(error) = self.open_local_document_path(kind, &path) {
            crate::logging::stderr_line!("aterm-gui: selected document was not opened: {error}");
            menu::notify("Couldn’t Open File", &error);
        }
    }

    /// The window whose find field owns text input, if any, for menu key equivalents
    /// that reach the app through AppKit rather than `on_key`.
    fn find_field_window(&self) -> Option<WindowId> {
        let wid = self.frontmost_window?;
        self.windows
            .get(&wid)
            .is_some_and(|ws| ws.search.is_some())
            .then_some(wid)
    }

    /// Keep a menu command from running through an open inline rename editor; returns
    /// whether the action was fully handled here. AppKit resolves a key equivalent
    /// before the first responder sees the key, so with the field focused ⌘V would
    /// paste into the PTY and ⌘W close the tab being renamed. Editing commands go to
    /// the field; everything else commits the edit first, so the command runs against
    /// a settled world. `RenameSession` is exempt, so re-issuing it stays idempotent.
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

    /// The licence fence ahead of a menu command, whose key equivalent never reaches
    /// `on_key`: a Paste is fenced as a paste press ([`press_fence`]), every other
    /// command takes the hand off.
    pub(crate) fn fence_menu_action(&mut self, action: menu::MenuAction) {
        match self.frontmost_window {
            Some(wid) if action == menu::MenuAction::Paste => {
                self.clear_move_license_keeping_hand(wid);
            }
            _ => self.clear_front_move_license(),
        }
    }

    /// Dispatch a menu-bar click into the `App` command method the matching keybinding
    /// already uses: the menu is an entry point, never a parallel implementation. `el`
    /// is needed only for items that must exit the loop or create a window.
    pub(crate) fn dispatch_menu_action(&mut self, el: &ActiveEventLoop, action: menu::MenuAction) {
        use menu::MenuAction;
        self.fence_menu_action(action);
        if self.divert_menu_action_around_rename(action) {
            return;
        }
        match action {
            // App menu --------------------------------------------------------
            // About and Software Update are routes inside the process-singleton
            // native Settings tab. Settings… (⌘,) focuses or creates that tab. "Open
            // aterm.toml" opens the file in the native assisted editor on every
            // platform; Help opens the project documentation.
            MenuAction::About => {
                let _ = self.open_settings_tab(crate::native_settings::SettingsRoute::About);
            }
            MenuAction::Version => {
                let _ = self.open_settings_tab(crate::native_settings::SettingsRoute::About);
            }
            MenuAction::SoftwareUpdate => {
                let _ = self.open_software_update_route_and_check();
            }
            // One-click update: applies a strictly-newer staged build now (re-exec
            // never returns), else opens the Software Update route.
            MenuAction::ApplyUpdate => self.apply_update_or_details(),
            // "Open aterm.toml" opens the native editor on the one config
            // file, as a message's Open aterm.toml does — never an external
            // editor, and never by way of a Settings page.
            MenuAction::Preferences => {
                if let Err(message) = self.open_config_editor_in_front() {
                    aterm_log::warn!("Open aterm.toml: {message}");
                }
            }
            MenuAction::Help => menu::open_help_url(),
            MenuAction::Quit => self.on_quit_requested(el),
            // File ------------------------------------------------------------
            // The first shell spawns off the loop (`tab_spawn`); the window is made
            // when it lands.
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
            // The Overview (`view.overview`): opened on the frontmost window,
            // which comes forward with it. No window resolvable is refused.
            MenuAction::Overview => {
                if let Err(e) = self.open_overview() {
                    self.pending_action_refusal = Some(e);
                }
            }
            // THE AGENTS MENU (`docs/DESIGN-owner-surfaces-2026-10-16.md` §4;
            // the App side lives in `app_take_over.rs`, `summarize.rs` and
            // `app_overview.rs`). The rows act on the FRONT window — the
            // "front tab is the subject" convention every other bar action
            // uses.
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
            // Take Over / Hand Back (`app_take_over.rs`): the person's hold
            // on the front window's focused session.
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
            // Agents ▸ Set Role…: the pin's editor over the active tab, one
            // field over (`meta role`).
            MenuAction::SetRole => {
                if let Some(wid) = self.frontmost_window {
                    let _ = self.begin_active_session_meta_edit(
                        wid,
                        crate::session_timeline::MetaField::Role,
                    );
                }
            }
            // Edit ▸ Reset Terminal (`crate::manual_reset`): queued on the session's
            // PTY reader, never waited for.
            MenuAction::ResetTerminal => {
                if let Some(wid) = self.frontmost_window {
                    self.menu_reset_terminal(wid);
                }
            }
            // View ▸ Show Needs in Windows: flip the live bit and persist it.
            MenuAction::ToggleNeedsInWindows => self.user_toggle_needs_in_windows(),
            // View ▸ Stats Bars: write the chosen form; the config apply
            // re-grids for a change into or out of the Deck.
            MenuAction::StatsBarsDeck | MenuAction::StatsBarsSill | MenuAction::StatsBarsOff => {
                if let Some(form) = action.stats_bars_form() {
                    self.user_set_stats_bars(form);
                }
            }
            // View ▸ Deck Size: write the chosen size; the config apply
            // re-decides every window's form and re-grids the ones it moves.
            MenuAction::DeckSizeAuto | MenuAction::DeckSizeFull | MenuAction::DeckSizeFolded => {
                if let Some(pref) = action.deck_size_pref() {
                    self.user_set_deck_size(pref);
                }
            }
            // View ▸ Stats Lens (⌘I): the keyboard lens on the front window.
            MenuAction::ToggleStatsLens => {
                let _ = self.toggle_stats_lens();
            }
            MenuAction::CloseTab => {
                // As Cmd-W; the window closes when its last tab did.
                if let Some(closed) = self.close_active_tab() {
                    self.close_window(el, closed);
                }
            }
            // Edit ------------------------------------------------------------
            // A menu item carries no originating window, so this is the
            // `frontmost_window` path (`Action::Copy` has a routed `wid`).
            MenuAction::Copy => {
                let _ = self.copy_selection();
            }
            // ⌘V under an open find belongs to the field: the key equivalent never
            // reaches `on_key`.
            MenuAction::Paste => match self.find_field_window() {
                Some(wid) => self.search_paste_in(wid),
                None => self.paste_clipboard(),
            },
            // Without a tab context (`invoke`, a palette row) these act on the
            // frontmost window's active tab, through the per-tab dispatcher.
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
            // Inert under an open find, which uses the terminal selection for its
            // match highlight.
            MenuAction::SelectAll => {
                if self.find_field_window().is_none() {
                    self.select_all();
                }
            }
            // The ⌘F key equivalent lands here, not in `on_key`; a native Settings
            // or Editor tab takes it as its own find.
            MenuAction::Find => self.find_requested(),
            // Step an open search, or resume the last accepted query (Cmd-G).
            MenuAction::FindNext | MenuAction::FindPrev => {
                let forward = matches!(action, MenuAction::FindNext);
                if let Err(why) = self.search_find_again(forward) {
                    self.pending_action_refusal = Some(why.to_string());
                }
            }
            // View ------------------------------------------------------------
            MenuAction::ToggleFullScreen => {
                if let Err(why) = self.toggle_fullscreen() {
                    self.pending_action_refusal = Some(why.to_string());
                }
            }
            // Font size — identical to on_key_font_zoom (⌘= / ⌘- / ⌘0).
            MenuAction::FontIncrease => self.set_font_size(self.font_size + FONT_ZOOM_STEP),
            MenuAction::FontDecrease => self.set_font_size(self.font_size - FONT_ZOOM_STEP),
            MenuAction::FontActualSize => self.set_font_size(self.default_font_size),
            MenuAction::SplitVertical => self.split_focused_pane(pane::SplitDir::Vertical),
            MenuAction::SplitHorizontal => self.split_focused_pane(pane::SplitDir::Horizontal),
            // The sink `toggle_matrix_rain` and `aterm ctl rain toggle` share.
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
            // A refusal goes to `pending_action_refusal`, which `invoke` reports.
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
            // Focus or create the process-singleton native Settings tab.
            MenuAction::ToggleSettings => {
                self.open_settings_tab(crate::native_settings::SettingsRoute::Home);
            }
            // Same singleton tab, opened at the Packages route — the menu-bar
            // path to the batteries-included toolchain surface.
            MenuAction::Packages => {
                self.open_settings_tab(crate::native_settings::SettingsRoute::Packages);
            }
            // …and at the Messages route, the log behind the message band.
            MenuAction::Messages => {
                self.open_settings_tab(crate::native_settings::SettingsRoute::Messages);
            }
            // …and at the Harness route — the bar and palette path to the
            // coding agent harness's page (the Autopilot menu opens it in the
            // window it was opened in: `App::open_harness_settings`).
            MenuAction::HarnessSettings => {
                if let Some(wid) = self.frontmost_window {
                    self.open_harness_settings(wid, crate::harness_screen::HarnessFocus::Top);
                }
            }
            MenuAction::OpenPalette => self.toggle_palette(),
            // The palette, scoped to its Theme section.
            MenuAction::ChooseTheme => self.open_theme_palette(),
            // …scoped to the front tab: its own theme, nothing written.
            MenuAction::ChooseTabTheme => self.open_tab_theme_palette(),
            MenuAction::ClearTabTheme => self.clear_front_tab_theme(),
            // …and Settings ▸ Themes, the Theme Showcase.
            MenuAction::BrowseThemes => {
                self.open_settings_tab(crate::native_settings::SettingsRoute::Themes);
            }
            // Window ----------------------------------------------------------
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
                // Zoom toggles maximised, like the green-button / Window ▸ Zoom.
                Some(w) => w.set_maximized(!w.is_maximized()),
                None => {
                    self.pending_action_refusal =
                        Some("no window is on screen to zoom".to_string());
                }
            },
        }
    }

    /// Dispatch one tab context-menu action against the exact tab it was popped on
    /// (`Wake::TabMenuAction`, from the macOS strip's `NSMenu`), which need not be the
    /// active one. `tab` is the stable id captured at pop; wakes are FIFO, so a tab
    /// mutation queued meanwhile has applied: a reorder re-targets the same tab and a
    /// closed one makes the action a logged no-op. Never fall back to a positional
    /// guess, which would act on the wrong session. Other actions fall through to
    /// [`Self::dispatch_menu_action`].
    pub(crate) fn dispatch_tab_menu_action(
        &mut self,
        el: &ActiveEventLoop,
        window: WindowId,
        tab: crate::tab_model::TabId,
        action: menu::MenuAction,
    ) {
        use menu::MenuAction;
        let Some(index) = self.tab_index_for_id(window, tab) else {
            // The clicked tab (or its window) went away; acting on another tab
            // would misdirect the action.
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
            // The AGENT section acts on the CLICKED tab's session, resolved
            // fresh here (as for Copy*), never the front tab the plain
            // dispatcher would pick. Take Over and Hand Back run now, in the
            // gesture's turn (`App::person_gesture_refused`).
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

    /// Dispatch one session-argument row of a tab's context menu —
    /// `Wake::TabMenuConnection { window, tab, peer_sid, verb }` — the AGENT
    /// section's rows (`docs/DESIGN-owner-surfaces-2026-10-16.md` §4). `tab` is
    /// the pop-time STABLE id, re-resolved here under the same
    /// no-positional-guess contract as [`Self::dispatch_tab_menu_action`];
    /// `peer_sid` is the row's snapshot session.
    ///
    /// * `Show` raises it (window focus + tab select) through the same body as
    ///   the `raise` wire verb ([`Self::raise_session_by_id`]): `Go to
    ///   <manager>`, a worker row.
    /// * `StopWorker` asks first — "Stop <tab>? Its agent gets SIGTERM and its
    ///   task reads exited." — on the platform's FAIL-CLOSED sheet (Cancel is
    ///   the default button: a Return typed elsewhere never stops anything),
    ///   then signals the agent in process ([`Self::stop_worker`], which reads
    ///   the task and the hold again when it acts). A stop it would refuse
    ///   (an ended task, no agent in front, a control client's gesture on a
    ///   held tab) is refused before any sheet ([`Self::stop_worker_refused`]
    ///   says it to whom it concerns). A CONTROL CLIENT'S choice of the row
    ///   (its keys into the tab menu) is the wire `signal` verb's act on the
    ///   tab it drives: it stops the worker under the gesture mark, refused
    ///   where the verb would be, and logged only — no sheet and no card
    ///   reach the person's glass for a robot's act. With no sheet to ask the
    ///   PERSON on (headless, off macOS) the Overview opens with the worker's
    ///   inline confirm armed ([`Self::overview_arm_stop`]): the person's
    ///   stop is never unconfirmed.
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
                // A CONTROL CLIENT'S CHOICE is the `signal` verb's act: no
                // sheet or card on the person's glass for a robot's act (the
                // alert rule), the stop under the mark, logged.
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
                // Only a real window has a sheet to ask on (the CLI rows'
                // rule): a headless path never reaches the platform's alert.
                let real_window = self
                    .windows
                    .get(&window)
                    .is_some_and(|ws| ws.os_window.is_some());
                let answer = if real_window {
                    self.apprt.confirm_owner("Stop Worker", &body, "Stop")
                } else {
                    None
                };
                // The test arm answers for the sheet a headless App has not.
                #[cfg(test)]
                let answer = self.agents.owner_sheet_for_test.unwrap_or(answer);
                match answer {
                    Some(true) => {
                        if let Err(why) = self.stop_worker(local) {
                            self.stop_worker_refused(local, why);
                        }
                    }
                    Some(false) => {}
                    // NO SHEET TO ASK THE PERSON ON (headless, off macOS):
                    // the confirm is the Overview's inline one, on this
                    // worker, armed for the person's hand. Their second ⌫
                    // stops it; nothing is stopped unconfirmed.
                    None => self.overview_arm_stop(window, local),
                }
            }
        }
    }

    /// The registry `sid` string for the terminal session labeling tab `index`
    /// of `window`, or `None` for a native tab / unregistered stub (the `Copy
    /// Session ID` payload). Takes the store read lock briefly.
    pub(crate) fn tab_session_id_text(&self, window: WindowId, index: usize) -> Option<String> {
        let session = self.tab_terminal_session(window, index)?;
        self.store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_local(session)
            .map(|h| h.sid.as_str().to_string())
    }

    /// The raw shell-reported cwd of the terminal session labeling tab `index`
    /// of `window` (never `~`-abbreviated: a pasted path must be real), or
    /// `None` when absent (the `Copy CWD` payload). One brief term lock.
    pub(crate) fn tab_session_cwd(&self, window: WindowId, index: usize) -> Option<String> {
        use crate::cwd_native::ReportedCwd as _;
        let session = self.tab_terminal_session(window, index)?;
        let s = self.pool.get(session)?;
        let term = crate::term_lock(&s.term);
        // The native form: `C:\…` on Windows, not OSC 7's URI path.
        term.native_working_directory()
            .filter(|c| !c.is_empty())
            .map(|c| c.into_owned())
    }

    /// The current position of the tab with stable id `tab` in `window`, or `None`
    /// when it no longer exists there: a stable id survives any reorder and is never
    /// reused, where a raw position would re-bind to whatever tab inherited the slot.
    pub(crate) fn tab_index_for_id(
        &self,
        window: WindowId,
        tab: crate::tab_model::TabId,
    ) -> Option<usize> {
        let ws = self.windows.get(&window)?;
        ws.tab_set.tabs().iter().position(|t| t.id == tab)
    }

    /// The terminal session labeling tab `index` of `window` (its focused leaf,
    /// as `tab_titles` resolves it), or `None` for a native tab / an
    /// out-of-range index.
    pub(crate) fn tab_terminal_session(&self, window: WindowId, index: usize) -> Option<u64> {
        let ws = self.windows.get(&window)?;
        let tab = ws.tab_set.tabs().get(index)?;
        self.view_store
            .get(tab.focus)
            .copied()
            .and_then(crate::tab_model::View::terminal_session)
    }

    /// Request a Serious Mode transition through the versioned config lane native
    /// Settings uses; the live policy changes only once the write is durable, so it
    /// can never run ahead of `aterm.toml`.
    pub(crate) fn user_toggle_serious_mode(&mut self) {
        if let Err(error) = self.queue_serious_mode_toggle() {
            self.post_message(crate::message_reporters::serious_mode_feedback(&format!(
                "Serious Mode was not changed: {error}"
            )));
        }
    }

    /// Open a compatibility GUI target through the menu items' paths: `prefs`,
    /// `about` and `update` resolve to native Settings routes, `menu` is a transient
    /// overlay, and the front window is always open.
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
                // The palette is own-rendered on the front window.
                self.palette_enter();
                Ok(())
            }
            AuxTarget::TabMenu => {
                // The front window's active-tab context menu, as the Menu key opens it;
                // refused for a native tab or a window with the strip off.
                let Some(wid) = self.frontmost_window else {
                    return Err("no front window to open the tab menu on".to_string());
                };
                if self.open_active_tab_context_menu(wid) {
                    Ok(())
                } else {
                    Err("the active tab has no session context menu".to_string())
                }
            }
            // The Overview: `open overview` (Owner-only at the dispatch)
            // opens it on the frontmost window with raise/focus; `controls
            // overview` / `window overview` then read it.
            AuxTarget::Overview => self.open_overview(),
            // The Autopilot menu, from the front window's focused session's
            // chip, as `↓` on the selected chip opens it; `controls
            // autopilot-menu` / `image` then read it.
            AuxTarget::AutopilotMenu => self.open_front_autopilot_menu(),
            AuxTarget::Front => {
                Err("the front terminal window is already open (use window/chrome)".to_string())
            }
            AuxTarget::Prefs | AuxTarget::About | AuxTarget::Update => {
                Err("native Settings alias routing failed".to_string())
            }
        }
    }

    /// The `open <target> close` counterpart; a not-open target is an idempotent `Ok`.
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
            AuxTarget::Overview => {
                if let Some(wid) = self.frontmost_window {
                    self.overview_exit(wid);
                }
                Ok(())
            }
            // `Front` is the terminal itself and is not a closable app surface here.
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

    /// Arm a minimal pending overlap, so the revoking form (epoch bump, cancel poke)
    /// is observable against the tolerated one.
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

    /// A forwarded key release is tolerated key traffic like its press: it neither
    /// bumps the activity epoch nor pokes a pending overlap's cancel channel.
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

    /// The App menu's "Open aterm.toml" opens the native editor on the
    /// isolated config file — the file itself, never a Settings page.
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

/// `[keybindings]` actions for commands that are otherwise menu items. Off
/// macOS there is no menu bar, so without an `Action` twin these would be
/// reachable only through the palette and could not be given a chord.
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

    /// `find_next` / `find_prev` reach the same reducer Edit ▸ Find ▸ Find Next drives,
    /// in both states: stepping an open bar, and reopening the last accepted
    /// query after Enter closed it (the state a chord is for; with the bar open
    /// the arrow keys already work).
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

    /// `select_all` selects the visible screen and, like the
    /// `MenuAction::SelectAll` arm it mirrors, stays inert while the find bar
    /// owns input (its match highlight is the terminal selection).
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
    //! The ledger key: ⇧⌘L with a session focused composes
    //! `aterm drive ledger @<sid> --format html --out <file>` for that session
    //! (headless: composed, not launched), and the bindable `open_ledger`
    //! action does the same thing.

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

        // The hardcoded chord.
        let fired =
            app.on_key_super_shift_chord(ModifiersState::SUPER | ModifiersState::SHIFT, &shift_l());
        assert!(fired, "⇧⌘L is consumed, never typed into the PTY");
        let plan = app.last_ledger_plan.clone().expect("a plan was composed");
        assert_eq!(plan.sid, stable);
        assert_eq!(plan.argv[..2], ["drive", "ledger"]);
        assert_eq!(plan.argv[2], format!("@{stable}"));
        assert_eq!(plan.argv[3..6], ["--format", "html", "--out"]);
        assert_eq!(plan.argv[6], plan.out.to_string_lossy());

        // Without Shift it is not the ledger key (Cmd-L is nothing).
        app.last_ledger_plan = None;
        assert!(!app.on_key_super_shift_chord(ModifiersState::SUPER, &shift_l()));
        assert!(app.last_ledger_plan.is_none());

        // The bindable action is the same method.
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
/// `keymap::build_key_input` keeps the keypad identity for the PTY encoders, so
/// the classifier receives `Numpad5` / `NumpadEnter` / `NumpadEnd`, and every
/// class here (turn boundary, typed glyph, nav hint, kill) must read as it does
/// for `Character('5')` / `Enter` / `End`. `PressClass` never gates bytes.
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

    /// KP_Enter is a submitted turn: turn boundary, `enter_like`,
    /// `keyed_enter`, the pet's Submit, forward momentum, as for `Enter`.
    /// Modified, it stays an application key like the main Shift+Enter.
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

    /// A keypad digit is a typed glyph: predictive-echo candidate, forward
    /// momentum, the cosmetic `typed` witness, the pet's Text.
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
        // Shift picks a keypad level, never the main row's shifted glyph: the
        // encoder writes '5' for Shift+KP_5 and '=' for Shift+KP_=, so that is
        // the echo to predict, not the US table's '%' and '+'.
        let shifted_ev = press(NamedKey::Numpad5, Modifiers::SHIFT);
        let shifted = classify_press(&shifted_ev);
        assert_eq!(shifted.typed, Some('5'));
        assert_eq!(shifted.predict_candidate, Some(PredictKey::Glyph('5')));
        let equal_ev = press(NamedKey::NumpadEqual, Modifiers::SHIFT);
        assert_eq!(classify_press(&equal_ev).typed, Some('='));
    }

    /// The edit keys the predictor models are a bare `←` or `→` and Backspace: a
    /// modified arrow is a word motion, a selection or the window's. The keypad's
    /// arrows (NumLock off) are edit keys too: the legacy encoder sends
    /// `NumpadArrowLeft` exactly as `ArrowLeft` (`ESC [ D`, DECKPAM or not), and
    /// under a Kitty mode that gives the keypad its own code the press path
    /// routes it as input the predictor does not model. A lock is no chord: an
    /// arrow with Caps Lock or Num Lock on is still bare (the legacy encoder
    /// drops locks; Kitty's case is the press path's), but a lock beside a chord
    /// is still that chord.
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

    /// The LEARNED keys the predictor places at an authenticated prompt: Home,
    /// End and Delete of the main block, bare (a lock is no chord), and Ctrl-A
    /// and Ctrl-E, Ctrl alone. The keypad's Home is another key (`ESC O w`
    /// under DECKPAM), and Shift, Alt or Super on any of them is a chord the
    /// predictor does not model; so is Ctrl with Shift or Alt on `a`.
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

    /// NumLock off, the keypad is a second navigation block: its arrows and
    /// Home/End carry the nav hint and KP_Delete is a (stationary, word-tier)
    /// kill key, as their main-block twins are.
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

    /// The negative controls: KP_Begin has no main-block twin and classifies
    /// as nothing, and the main-block keys are untouched by the fold.
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

    /// A `MouseMove` reaching the seam keeps the pane-local `last_mouse_cell` that
    /// `on_cursor_moved` published (a follow-up press or wheel reads it).
    #[test]
    fn mouse_move_does_not_clobber_pane_local_cell() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        // Pretend `on_cursor_moved` already mapped the pointer to pane-local (5, 7).
        app.windows.get_mut(&wid).unwrap().last_mouse_cell = (5, 7);
        // A hover MouseMove whose row/col differ from the published pane-local cell.
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

/// Tier-1 conformance for the process-wide physical press owner. The derived
/// model proves the complete bounded protocol; this module binds the five
/// shipping ownership/routing seams to its `Next` relation while a real
/// two-window `App` changes focus between press, repeat, and release.
#[cfg(test)]
#[path = "app_input_input_release_pairing_conformance_tests.rs"]
pub(crate) mod input_release_pairing_conformance;

/// On every route the host drives (`paste_order::routes_for_test`): an encoded key
/// is the stub sink's synchronous `WriteFailed` inline, and on the ConPTY route (every
/// key on Windows) an `Ok` with one job handed to the ordered writer.
#[cfg(test)]
#[path = "app_input_overlay_gate_pairing_tests.rs"]
mod overlay_gate_pairing_tests;

#[cfg(test)]
#[path = "app_input_smooth_scroll_tests.rs"]
mod smooth_scroll_tests;

#[cfg(test)]
#[path = "app_input_keystroke_press_side_effect_tests.rs"]
mod keystroke_press_side_effect_tests;

/// Lock-elision guards on the press/release path. The terminal mutex is the one
/// lock a keystroke shares with the PTY reader, so the press path takes it
/// exactly once (the seam's consolidated scope) and a byte-silent release takes
/// it not at all, while every human-visible side effect still happens.
#[cfg(test)]
#[path = "app_input_press_path_lock_elision_tests.rs"]
mod press_path_lock_elision_tests;

/// The layout's character on a text press (Caps Lock, Shift on a non-US
/// layout) and Ctrl on a non-Latin one, through the shipping `on_key` — on the
/// backends whose key is the layout's unshifted key (macOS, X11/Wayland).
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

        // Start from the dangerous state: this line has already proved a slow
        // echo, so an Adaptive guess is both pending and visible before Codex
        // takes ownership of the composer.
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

        // Codex's observed main-screen flags are 1|2|4: disambiguate, report event
        // types, and report alternate keys. It does not set REPORT_ALL_KEYS_AS_ESC,
        // so a gate keyed only on that flag would arm and later erase ghost text.
        term_lock(&term).process(b"\x1b[>7u");
        assert!(model.fire("EnterComposer", &mut model_state));
        // Pressed while the guess is in flight, not wherever load has pushed the
        // wall clock: past its glitch window it would be idle by expiry alone.
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

/// TYPED-WORD SUMMON plumbing. The detector's own laws (once-per-completion,
/// cooldown without restamp, backspace tolerance, session keying) are proven
/// in `aterm_effects::kitty_summon`; this module binds the App seams: the press path
/// feeds ONLY typed keys (never screen bytes), a granted feline completion
/// reaches the Kitty Log's ordinary episode rules and NOTHING else (the cat the
/// user sees is the ambient word-cat the echoed word grows), the canine arm
/// pops its visiting dog, and the effects master gate keeps everything inert
/// when nothing could draw.
#[cfg(test)]
#[path = "app_input_typed_kitty_summon_tests.rs"]
mod typed_kitty_summon_tests;

/// Predictive echo at prompts that do not echo, end to end through the press
/// path and the composed present: no mode paints a keystroke the program will
/// not echo (a canonical no-echo tty, a prompt scrolled onto an echo-proven
/// row, a password typed ahead after any submit key).
#[cfg(test)]
#[path = "app_input_predict_privacy_tests.rs"]
mod predict_privacy_tests;

/// Kitty commands: host wiring of the typed-line listener
/// (`aterm_effects::typed_tricks`): the cascade order, the admission gates, and the
/// pet and flash mappings.
#[cfg(test)]
#[path = "app_input_typed_trick_tests.rs"]
mod typed_trick_tests;

/// Favourite-this-kitty App seams (the ledger laws are proven in `kitty_log`): which
/// cat is promoted, the two gates, and the surface wiring (`action_by_name`, since
/// `dispatch_menu_action` needs an event loop).
#[cfg(test)]
#[path = "app_input_favourite_kitty_tests.rs"]
mod favourite_kitty_tests;

/// The homecoming rate law's App half (the gate's laws are proven in
/// `aterm_effects::companion_look`): the rung report at verdict time, the arrival
/// mapping at the pet sync sites, and a hello spent only by a drawn present that put
/// a genuinely differing pair on the pet.
#[cfg(test)]
#[path = "app_input_homecoming_rate_law_tests.rs"]
mod homecoming_rate_law_tests;

/// Keyboard parity for the tab context menu, driven through the shipping
/// `on_key` routing with real winit events. Windows and Linux, gated like the
/// chord arm itself (macOS's native strip carries a real `NSMenu` and compiles
/// the arm out).
#[cfg(all(test, any(windows, target_os = "linux")))]
#[path = "app_input_tab_menu_key_tests.rs"]
mod tab_menu_key_tests;

/// The control seam (`ctl key menu`, `ctl key down`, `ctl mouse wheel`) pops,
/// dismisses and swallows for the tab menu as the glass does.
#[cfg(all(test, any(windows, target_os = "linux")))]
#[path = "app_input_tab_menu_seam_tests.rs"]
mod tab_menu_seam_tests;

/// The find field through the shipping `on_key` with real winit events: key
/// classification, repeat ownership and the `App`-side edit.
#[cfg(test)]
#[path = "app_input_find_field_key_tests.rs"]
mod find_field_key_tests;

/// A pane-focus chord on an arrow claims its key only while a pane lies that way:
/// on a one-pane tab, a zoomed pane or at the edge of a split, Alt+←/→ reaches the
/// program as word motion, booked like any forwarded press. Off the arrows
/// (`ctrl+j`) the chord always keeps its key.
#[cfg(test)]
#[path = "app_input_pane_focus_fall_through_tests.rs"]
mod pane_focus_fall_through_tests;

/// The window's key layers on real winit events: the chords the GUI claims
/// before the program (Windows' PSReadLine chords, the scrollback chords on the
/// alternate screen) and the keys a non-US Windows layout builds.
#[cfg(test)]
#[path = "app_input_key_layer_tests.rs"]
mod key_layer_tests;

/// The `tone` control verb's App half ([`App::tone_status`]): the readout that
/// makes an audible-only feature diagnosable. Each test pins one state, since
/// by ear they are all identical silence.
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

    /// A muted build: the window still tracks typed text (so a later unmute
    /// classifies a coherent window) but the classifier never ran, and
    /// `audio=inert active=false inferences=0` names the gate that stopped it.
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

    /// A live build: typing reports the classified mood, the same value on
    /// `effective=` (what the synth is told), and a nonzero inference count.
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

    /// A disabled knob over a live audio host: the verdict is still reported,
    /// `effective=technical` shows the synth gets the neutral identity, and
    /// `active=false` explains why the verdict will go stale.
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

    /// No window to report on is a refusal, never a fabricated neutral row.
    #[test]
    fn a_windowless_app_refuses_rather_than_inventing_a_row() {
        let mut app = App::headless_for_test();
        app.frontmost_window = None;
        assert!(app.tone_status().is_err());
    }

    /// The typed window text never reaches the wire: the tracker is fed from
    /// the committed key path, so it may hold characters typed at an unechoed
    /// password prompt, which `text`/`screen` cannot see either.
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
        // No partial leak either: no run of four consecutive characters from
        // the secret may appear anywhere on the row.
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

    /// One real frame through `cursor_frame`, then the real verb. The window's
    /// seam is resolved only by a tick, so every seam reading is taken after one.
    fn seam_row_after_one_frame(app: &mut App) -> String {
        let wid = WindowId(0);
        let fx = crate::app_render::CursorFxInputs::sample_for_test(std::time::Instant::now());
        app.cursor_frame(wid, fx).expect("one cursor frame");
        app.tone_status().expect("the fixture window is focused")
    }

    /// A trail-on, sounds-on window on a live audio host, before any
    /// per-test knob.
    fn audible_app(style: &str) -> App {
        let mut app = App::headless_for_test();
        app.trail_audio = TrailAudio::capturing_for_test();
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some(style.into());
        app.config.trail_sounds = Some(true);
        app
    }

    /// An aurora dimmed to zero is silent, and the verb names that gate rather than
    /// `seam=closed:engine-silent trail=on`, which is reserved for a regression.
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

    /// The verb reads the resolved config, never the folded one: a trail dark
    /// only because of macOS Reduce Motion is still `trail=on`, and a key
    /// sounds.
    #[test]
    fn a_reduce_motion_trail_is_dark_but_on_and_heard() {
        let mut app = audible_app("rainbow kitty");
        app.system_reduce_motion = true;
        let line = seam_row_after_one_frame(&mut app);
        assert!(line.contains("seam=open"), "{line}");
        assert!(line.contains("trail=on"), "{line}");
        assert!(line.contains("engine_sound=open"), "{line}");
    }

    /// The classic wake through the real frame and verb: a fresh `classic`
    /// window clicks and the verb does not read `engine-silent`.
    #[test]
    fn a_classic_window_is_heard_and_the_verb_says_so() {
        let mut app = audible_app("classic");
        let line = seam_row_after_one_frame(&mut app);
        assert!(line.contains("seam=open"), "{line}");
        assert!(line.contains("engine_sound=open"), "{line}");
        assert!(line.contains("trail=on"), "{line}");
    }
}

/// The inline rename field through the shipping `on_key` with real winit events: the
/// early gate that keeps ⎋ and `[key_sequences]` bytes off the PTY, the field edit,
/// the commit/cancel exits, and the settle-and-fall-through rule.
#[cfg(test)]
#[path = "app_input_rename_field_key_tests.rs"]
mod rename_field_key_tests;

/// The person at the keyboard (`human_ms=`, `EVENT <local> human`): the window's
/// input seam stamps it for typing and pointing, a control verb never does;
/// driven through `App::input`, which both reach.
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

    /// What a person reads back or selects with: a click, a drag, a wheel notch,
    /// a view scroll. A supervisor's key would snap that view and clear that
    /// selection.
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

    /// Every typing or pointing event a control verb can produce leaves
    /// `human_ms=-` and records nothing; the same events from the window's
    /// handlers stamp it (Esc included, and a scroll back or a selecting click).
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

    /// A burst of typing and pointing is one `human` event; what is not a hand
    /// on the session (a key release, a hover, a focus report) stamps nothing.
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

        // The first cluster is three bytes (e + combining acute), the emoji
        // cluster is eleven bytes. These are winit byte offsets, not UTF-16.
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

        // Negative control: a bridge that delivers None moves a middle caret to
        // the end of the marked text.
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

        // The secret field intentionally has no marked-text projection. Drive
        // real physical-key routing between platform preedit and commit, then
        // verify ordinary input and an explicitly cleared composition still type.
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
        app.dispatch_native_event(
            wid,
            AppEvent::Action(ActionInvocation {
                id: crate::native_ui::ActionId::new("system-state/apply"),
                value: None,
            }),
        )
        .unwrap();
        let Some(crate::system_state::SystemStateAction::Apply(answer)) =
            app.native_runtime.take_system_state_action()
        else {
            panic!("reviewed form queues its one answer");
        };
        let crate::workstation_setup::WorkstationSetupDecision::Begin(plan) = answer.decision
        else {
            panic!("Apply carries the edited plan");
        };
        assert_eq!(plan.vault_url, "語nn");
        assert_eq!(answer.password.unwrap().as_str(), Some("語nn"));
    }
}

/// The refused-input row's fold: a burst of one cause is one row, and a new
/// cause replaces that row's line under the same key. `Unavailable` has no live path (it needs the input
/// writer gone), so it is raised directly here.
#[cfg(test)]
mod refusal_row_tests {
    use super::{InputRefusal, paste_order};

    /// A paste the ordered writer refused because its queue is full now answers
    /// `ERR busy input-queue-full`, as a key refused at the cap does on every route;
    /// one too large for the queue, or a writer gone, is a failed write (a retry as
    /// is cannot cure it).
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

/// `input_release_pairing` actions no shipping seam performs, each waived where its
/// Tier-1 (`input_release_pairing_conformance`) binds it.
// The machine has 21 actions: twelve carry `#[refines]` anchors on their
// `app_input.rs` seams, and these nine are explicit scope boundaries.
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
