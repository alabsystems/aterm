//! Added by the aterm project in 2026; see the repository NOTICE.
//!
//! The Wayland CLIPBOARD and PRIMARY selections — a clipboard API upstream winit
//! does not have, over the sctk data device this backend already binds per seat
//! for drag-and-drop, plus a primary-selection device bound beside it.
//!
//! WHY (aterm, 2026-09-15): on a Wayland session with no XWayland the GUI's only
//! clipboard was X11, so every copy, paste, copy-on-select and OSC 52 was inert
//! while the surface reported success (glass hunt, m17-tower, 2026-09-01, #1).
//! A compositor without a data-control protocol (GNOME's Mutter here advertises
//! `wl_data_device_manager` and `zwp_primary_selection_device_manager_v1`, no
//! `ext_data_control`) lets only the client that RECEIVED input set a selection,
//! and only with a serial from that input — which is why this lives inside the
//! event loop, next to the seat that has the serials, and not in a helper.
//!
//! SHAPE: [`WaylandClipboard`] is a cloneable, thread-safe handle. A `copy` stores
//! the text in the shared own-selection slot at once (so an own-read answers
//! synchronously, on any thread) and posts a request the loop thread turns into
//! `set_selection` with the seat's latest serial — after RELEASING the selection
//! it already holds, and only when the compositor will take a claim at all. Both
//! are laws with a measurement behind them: see [`CLAIM_ORDER`] and
//! [`SelectionPort::copy`]. The loop answers each copy with the [`CopyOutcome`] it
//! reached, so a caller off the loop thread learns that a copy was REFUSED instead
//! of being told `true` for a write the compositor dropped.
//!
//! A `paste` answers from that slot
//! when we own the selection; otherwise it asks the loop for the foreign offer's
//! pipe, which a helper thread reads so the loop never blocks on a slow source.
//! A paste of a foreign selection from the LOOP thread itself would deadlock the
//! loop on its own reply, so it answers `None` there — callers run foreign reads
//! on a worker, exactly as they already do for the X11 backend.

use std::io::{Read, Write};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use sctk::data_device_manager::data_source::CopyPasteSource;
use sctk::data_device_manager::{ReadPipe, WritePipe};
use sctk::primary_selection::device::PrimarySelectionDeviceHandler;
use sctk::primary_selection::selection::{PrimarySelectionSource, PrimarySelectionSourceHandler};
use sctk::reexports::calloop::channel::Sender;
use sctk::reexports::client::backend::ObjectId;
use sctk::reexports::client::protocol::wl_data_source::WlDataSource;
use sctk::reexports::client::{Connection, QueueHandle};
use sctk::reexports::protocols::wp::primary_selection::zv1::client::zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1;
use sctk::reexports::protocols::wp::primary_selection::zv1::client::zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1;
use tracing::{debug, warn};

use super::state::WinitState;

/// Which selection a request addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WaylandSelection {
    /// The CLIPBOARD: explicit copy / paste.
    Clipboard,
    /// The PRIMARY selection: select-to-copy / middle-click paste.
    Primary,
}

/// The text mime types a copy offers, and the order a paste prefers them in.
const TEXT_MIMES: [&str; 5] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "TEXT", "STRING"];

/// A foreign paste larger than this is cut off: a terminal paste is text, and an
/// unbounded read of a hostile source must not exhaust memory.
const MAX_PASTE_BYTES: usize = 16 << 20;

/// How long a paste waits for a foreign source (the request, the pipe and the
/// read all fit inside it on a healthy compositor; a hung owner is refused).
const PASTE_TIMEOUT: Duration = Duration::from_secs(3);

/// How long a copy made OFF the loop thread waits for the loop's verdict. The
/// loop answers from a channel callback, so this is only ever spent when the loop
/// is wedged — and a caller that waited in vain is told `false`, never `true`.
const COPY_TIMEOUT: Duration = Duration::from_secs(3);

/// The two protocol steps a copy takes at the compositor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaimStep {
    /// Give up the selection this client already offers, if it offers one.
    Release,
    /// Offer the new source, backed by the seat's newest input serial.
    Claim,
}

/// THE ORDER, as data rather than as prose, so a test can machine-check it and
/// [`SelectionPort::copy`] can only obey it: RELEASE first, CLAIM second.
///
/// WHY (aterm, 2026-09-16): a compositor drops a `set_selection` whose serial is
/// not NEWER than the serial behind the selection this client already holds —
/// silently, with no protocol error. A copy with no fresh input behind it can only
/// re-spend the serial it already has: OSC 52 from the shell, `aterm ctl copy`,
/// copy-on-select after the click's serial is spent, a second Ctrl-C on an
/// unchanged selection. Claiming FIRST therefore loses those copies, and the
/// release that followed made it worse than a no-op — dropping our standing source
/// handed the selection to the compositor's cached copy of the PREVIOUS text, so
/// the clipboard silently went BACKWARDS while this client's own slot already held
/// the new text.
///
/// Measured on GNOME Shell 50.1 (m17-tower, Ubuntu 26.04, Wayland, 2026-09-16):
/// four `aterm ctl copy` calls with no input between them, the clipboard read
/// back by a separate client — copies 1 and 3 landed, copies 2 and 4 left the
/// text of the copy before them, strictly alternating, because each refused claim's own release
/// cleared the way for the next one. Releasing first leaves nothing for the
/// compositor to call the claim superseded, and the same frozen serial is accepted
/// every time: 4 of 4.
pub(crate) const CLAIM_ORDER: [ClaimStep; 2] = [ClaimStep::Release, ClaimStep::Claim];

/// What a copy reached at the compositor. A copy is asynchronous — the loop thread
/// does the protocol — so this is what the loop hands back, and what decides
/// whether the caller is told the text was placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyOutcome {
    /// A fresh source is the selection: every other client was told the selection
    /// changed, and reads the new text from us.
    Claimed,
    /// The compositor would not take a fresh claim, but we still hold the selection
    /// from an earlier copy and KEPT it. Our source serves the shared slot, which
    /// already holds the new text, so a paste still reads the new text — and the
    /// clipboard is not walked backwards to the text before it.
    Kept,
    /// Nothing of ours is on the selection: the text was NOT placed.
    Refused(CopyRefusal),
}

/// Why a copy placed nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyRefusal {
    /// No seat has ever given this client an input serial, so there is no serial to
    /// spend and the compositor would refuse any claim.
    NoSerial,
    /// The compositor binds no device for this selection — the only refusal the
    /// protocol lets us read off the call itself.
    NoDevice,
    /// The seat's keyboard is on another client's surface, and we hold no selection
    /// to fall back on. A compositor hands the selection only to the client its
    /// keyboard is on, so the claim would be dropped in silence.
    Unfocused,
}

impl CopyOutcome {
    /// Whether the text is what another client now reads from this selection.
    pub(crate) fn placed(self) -> bool {
        !matches!(self, Self::Refused(_))
    }
}

/// A request from any thread to the loop thread.
pub(crate) enum ClipboardRequest {
    /// `ack` is `Some` for a caller that waits for the verdict; `None` for one that
    /// cannot (a copy made ON the loop thread — see [`WaylandClipboard::copy`]).
    Copy { selection: WaylandSelection, ack: Option<mpsc::Sender<CopyOutcome>> },
    Paste { selection: WaylandSelection, reply: mpsc::Sender<Option<String>> },
}

/// The own-selection slots, shared between the handle (any thread) and the loop.
/// `Some(text)` means this client owns that selection and `text` is what it
/// serves; `None` means a foreign client owns it (or nobody does).
#[derive(Default)]
pub(crate) struct Shared {
    clipboard: Option<String>,
    primary: Option<String>,
}

impl Shared {
    fn slot(&mut self, selection: WaylandSelection) -> &mut Option<String> {
        match selection {
            WaylandSelection::Clipboard => &mut self.clipboard,
            WaylandSelection::Primary => &mut self.primary,
        }
    }
}

/// The loop-thread side: the sources this client currently offers.
#[derive(Default)]
pub(crate) struct ClipboardState {
    shared: Arc<Mutex<Shared>>,
    clipboard_source: Option<CopyPasteSource>,
    primary_source: Option<PrimarySelectionSource>,
}

impl ClipboardState {
    pub(crate) fn shared(&self) -> Arc<Mutex<Shared>> {
        Arc::clone(&self.shared)
    }
}

/// The public handle: clone it anywhere, call it from any thread.
#[derive(Clone)]
pub struct WaylandClipboard {
    sender: Arc<Mutex<Sender<ClipboardRequest>>>,
    shared: Arc<Mutex<Shared>>,
    loop_thread: ThreadId,
}

impl std::fmt::Debug for WaylandClipboard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (clipboard, primary) = self
            .shared
            .lock()
            .map(|s| (s.clipboard.is_some(), s.primary.is_some()))
            .unwrap_or((false, false));
        f.debug_struct("WaylandClipboard")
            .field("owns_clipboard", &clipboard)
            .field("owns_primary", &primary)
            .finish()
    }
}

impl WaylandClipboard {
    pub(crate) fn new(
        sender: Sender<ClipboardRequest>,
        shared: Arc<Mutex<Shared>>,
        loop_thread: ThreadId,
    ) -> Self {
        Self { sender: Arc::new(Mutex::new(sender)), shared, loop_thread }
    }

    fn send(&self, request: ClipboardRequest) -> bool {
        self.sender.lock().map(|s| s.send(request).is_ok()).unwrap_or(false)
    }

    /// Place `text` on `selection`. The own slot is written at once; the loop takes
    /// selection ownership at its next turn and answers with the [`CopyOutcome`] it
    /// reached — `false` means the text is NOT on the system clipboard, so a caller
    /// (`aterm ctl copy`, the OSC 52 worker) reports a refusal instead of success.
    ///
    /// ON THE LOOP THREAD the verdict cannot be waited for: the loop only reads the
    /// request channel between turns, so a wait here would block the very thread
    /// that has to answer it, exactly as a foreign [`Self::paste`] would. Those
    /// callers (the GUI copy shortcut, copy-on-select) get `true` for a request the
    /// channel accepted — and they are the callers a refusal cannot reach: a copy
    /// driven by a keystroke, or by a click on our own surface, has by construction
    /// the keyboard focus and the fresh serial the compositor asks for. Should one
    /// be refused anyway, the loop still empties the own slot, so the next paste
    /// answers with what the clipboard holds rather than with text that never left.
    pub fn copy(&self, selection: WaylandSelection, text: &str) -> bool {
        if let Ok(mut shared) = self.shared.lock() {
            *shared.slot(selection) = Some(text.to_owned());
        }
        let on_loop = std::thread::current().id() == self.loop_thread;
        let (ack, verdict) = if on_loop {
            (None, None)
        } else {
            let (ack, verdict) = mpsc::channel();
            (Some(ack), Some(verdict))
        };
        if !self.send(ClipboardRequest::Copy { selection, ack }) {
            if let Ok(mut shared) = self.shared.lock() {
                *shared.slot(selection) = None;
            }
            return false;
        }
        match verdict {
            Some(verdict) => copy_verdict(&verdict),
            None => true,
        }
    }

    /// The text of `selection` when THIS client owns it — instant, any thread —
    /// `None` when a foreign client owns it (or it is empty).
    pub fn paste_owned(&self, selection: WaylandSelection) -> Option<String> {
        self.shared
            .lock()
            .ok()
            .and_then(|mut shared| shared.slot(selection).clone())
            .filter(|text| !text.is_empty())
    }

    /// The text of `selection`, ours or a foreign client's. BLOCKS (bounded) on a
    /// foreign owner, so call it off the event-loop thread; on the loop thread a
    /// foreign read answers `None` rather than deadlocking the loop on its own reply.
    pub fn paste(&self, selection: WaylandSelection) -> Option<String> {
        if let Some(own) = self.paste_owned(selection) {
            return Some(own);
        }
        if std::thread::current().id() == self.loop_thread {
            debug!("wayland clipboard: a foreign {selection:?} read on the loop thread answers None");
            return None;
        }
        let (reply, receiver) = mpsc::channel();
        if !self.send(ClipboardRequest::Paste { selection, reply }) {
            return None;
        }
        receiver.recv_timeout(PASTE_TIMEOUT).ok().flatten().filter(|text| !text.is_empty())
    }
}

/// The loop's answer to one copy, as the caller's `bool`. A verdict that never
/// arrives — the loop died holding the request, or is wedged past [`COPY_TIMEOUT`]
/// — is NOT success: the one thing this must never do is answer `true` for a copy
/// nobody can say reached the compositor.
fn copy_verdict(verdict: &mpsc::Receiver<CopyOutcome>) -> bool {
    match verdict.recv_timeout(COPY_TIMEOUT) {
        Ok(outcome) => outcome.placed(),
        Err(_) => false,
    }
}

/// Write `bytes` to a requester's pipe on a helper thread. The pipe may be
/// non-blocking (the requester made it), so `WouldBlock` is waited out; a closed
/// reader (`EPIPE`) ends the write — the requester gave up, nothing to do.
fn write_on_thread(mut pipe: WritePipe, bytes: Vec<u8>) {
    let _ = std::thread::Builder::new().name("wayland-clipboard-send".into()).spawn(move || {
        let deadline = Instant::now() + PASTE_TIMEOUT;
        let mut at = 0;
        while at < bytes.len() {
            match pipe.write(&bytes[at..]) {
                Ok(0) => break,
                Ok(n) => at += n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                },
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {},
                Err(_) => break,
            }
        }
        let _ = pipe.flush();
    });
}

/// Read a foreign offer's pipe to EOF on a helper thread and answer `reply`.
fn read_on_thread(mut pipe: ReadPipe, reply: mpsc::Sender<Option<String>>) {
    let _ = std::thread::Builder::new().name("wayland-clipboard-recv".into()).spawn(move || {
        let deadline = Instant::now() + PASTE_TIMEOUT;
        let mut bytes = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    bytes.extend_from_slice(&buf[..n]);
                    if bytes.len() >= MAX_PASTE_BYTES {
                        bytes.truncate(MAX_PASTE_BYTES);
                        break;
                    }
                },
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                },
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {},
                Err(_) => break,
            }
        }
        let _ = reply.send(String::from_utf8(bytes).ok().filter(|s| !s.is_empty()));
    });
}

/// The mime a paste asks a foreign offer for: the first of [`TEXT_MIMES`] it
/// advertises, so a UTF-8 offer is read as UTF-8 and a legacy one still reads.
pub(crate) fn pick_text_mime(advertised: &[String]) -> Option<String> {
    TEXT_MIMES.iter().find(|want| advertised.iter().any(|m| m == *want)).map(|m| (*m).to_owned())
}

/// The four things a copy does at ONE seat's selection, behind a seam: the
/// production side is [`WinitSelectionPort`] over the real devices, and a test
/// implements the same four against a compositor model, so the SEQUENCE a copy
/// sends is a thing that can be measured off the wire AND asserted in a unit test.
pub(crate) trait SelectionPort {
    /// Whether this client holds the seat's keyboard focus.
    fn focused(&self) -> bool;

    /// Whether this client currently offers a source for `selection`.
    fn offers(&self, selection: WaylandSelection) -> bool;

    /// Give up the source we offer for `selection`; a no-op when we offer none.
    fn release(&mut self, selection: WaylandSelection);

    /// Offer a fresh source for `selection` and hand it to the seat with `serial`.
    /// `false` when the compositor binds no device for this selection — the only
    /// refusal the call itself can report.
    fn claim(&mut self, selection: WaylandSelection, serial: u32) -> bool;

    /// THE COPY. One body, for production and for the test, so the order and the
    /// gate below are not things a call site can get wrong twice.
    ///
    /// Two compositor rules, both measured by reading the clipboard back from a
    /// SEPARATE client after each copy (GNOME Shell 50.1, m17-tower, Ubuntu
    /// 26.04, Wayland, 2026-09-16):
    ///
    /// 1. a claim whose serial is not NEWER than the serial behind the selection
    ///    this client already holds is dropped, and the drop is not readable at the
    ///    call — no error, no return value, and the `wl_data_device.selection` that
    ///    does carry the answer arrives on a later turn. Four copies re-spending
    ///    one serial landed on 1 and 3 and left the text of the copy before them on
    ///    2 and 4. That is [`CLAIM_ORDER`]: release first, and the claim has
    ///    nothing to supersede.
    /// 2. a claim from a client the seat's keyboard has LEFT is dropped the same
    ///    way. Unfocused, an `aterm ctl copy` left the previous text while holding
    ///    the selection itself, and left a THIRD-PARTY client's text untouched
    ///    while holding nothing — so it is the focus, not the serial, that decides.
    ///
    /// Rule 2 is why the release is gated rather than unconditional. Releasing a
    /// selection we hold, in front of a claim the compositor will drop, is what
    /// walks the clipboard BACKWARDS: the bytes fall back to the compositor's
    /// cached copy of the previous text. Keeping the source instead costs nothing
    /// and gains the copy — the source we already hold serves the shared slot,
    /// which the caller wrote the new text into before this ran, so the new text is
    /// what a paste reads ([`CopyOutcome::Kept`]). Holding nothing, there is
    /// nothing to keep and nothing that could work: say so ([`CopyRefusal::Unfocused`])
    /// rather than mint a source we would then wrongly believe was the selection.
    ///
    /// WHY THE ANSWER IS NOT READ BACK OFF THE WIRE INSTEAD. The
    /// `wl_data_device.selection` that follows a claim does say whose offer won —
    /// ours (five mimes) or a foreign one. But a compositor sends it only to the
    /// client its keyboard is on, so it cannot answer for rule 2, the case this
    /// gate exists for; and where it CAN be read, the order above has already made
    /// the claim land. A verification pass would spend a round-trip on every copy
    /// to confirm what the order guarantees, and still be silent exactly where the
    /// silence hurt.
    fn copy(&mut self, selection: WaylandSelection, serial: u32) -> CopyOutcome {
        if !self.focused() {
            return if self.offers(selection) {
                CopyOutcome::Kept
            } else {
                CopyOutcome::Refused(CopyRefusal::Unfocused)
            };
        }
        // The steps go in [`CLAIM_ORDER`] and nowhere else: the ORDER is half the
        // fix, so it is read from the constant the test pins rather than spelled
        // out again here, where a later edit could quietly re-invert it.
        let mut taken = false;
        for step in CLAIM_ORDER {
            match step {
                ClaimStep::Release => self.release(selection),
                ClaimStep::Claim => taken = self.claim(selection, serial),
            }
        }
        if taken {
            CopyOutcome::Claimed
        } else {
            CopyOutcome::Refused(CopyRefusal::NoDevice)
        }
    }
}

/// [`SelectionPort`] over the real thing: one seat of the event loop's state.
///
/// The seat is held by OBJECT ID, not by reference, so [`SelectionPort::release`]
/// can take the standing source out of the state before [`SelectionPort::claim`]
/// borrows the seat again; a reference held across both steps would pin it.
struct WinitSelectionPort<'a> {
    state: &'a mut WinitState,
    seat: ObjectId,
    qh: QueueHandle<WinitState>,
}

impl SelectionPort for WinitSelectionPort<'_> {
    fn focused(&self) -> bool {
        self.state
            .seats
            .get(&self.seat)
            .is_some_and(super::seat::WinitSeatState::holds_keyboard_focus)
    }

    fn offers(&self, selection: WaylandSelection) -> bool {
        match selection {
            WaylandSelection::Clipboard => self.state.clipboard.clipboard_source.is_some(),
            WaylandSelection::Primary => self.state.clipboard.primary_source.is_some(),
        }
    }

    /// Dropping the source IS the release: in the pinned smithay-client-toolkit
    /// 0.19.2, `CopyPasteSource` (`data_device_manager/data_source.rs:127`) and
    /// `PrimarySelectionSource` (`primary_selection/selection.rs:54`) each destroy
    /// their proxy in `Drop`, so the `inner().destroy()` that used to stand beside
    /// the drop only ever queued a second destroy for wayland-rs to discard.
    fn release(&mut self, selection: WaylandSelection) {
        match selection {
            WaylandSelection::Clipboard => drop(self.state.clipboard.clipboard_source.take()),
            WaylandSelection::Primary => drop(self.state.clipboard.primary_source.take()),
        }
    }

    fn claim(&mut self, selection: WaylandSelection, serial: u32) -> bool {
        let seat = &self.seat;
        match selection {
            WaylandSelection::Clipboard => {
                let device = self.state.seats.get(seat).and_then(|seat| seat.data_device.as_ref());
                match (self.state.data_device_manager_state.as_ref(), device) {
                    (Some(manager), Some(device)) => {
                        let source = manager.create_copy_paste_source(&self.qh, TEXT_MIMES);
                        source.set_selection(device, serial);
                        self.state.clipboard.clipboard_source = Some(source);
                        true
                    },
                    _ => false,
                }
            },
            WaylandSelection::Primary => {
                let device =
                    self.state.seats.get(seat).and_then(|seat| seat.primary_device.as_ref());
                match (self.state.primary_selection_manager_state.as_ref(), device) {
                    (Some(manager), Some(device)) => {
                        let source = manager.create_selection_source(&self.qh, TEXT_MIMES);
                        source.set_selection(device, serial);
                        self.state.clipboard.primary_source = Some(source);
                        true
                    },
                    _ => false,
                }
            },
        }
    }
}

impl WinitState {
    /// The seat a copy speaks for: the one whose keyboard or pointer last gave
    /// this client a serial — the compositor validates `set_selection` against it.
    /// Answered by OBJECT ID rather than by reference, so the caller can take the
    /// standing source out of `self` before it borrows the seat again.
    fn clipboard_seat(&self) -> Option<(ObjectId, u32)> {
        self.seats
            .iter()
            .map(|(id, seat)| (id.clone(), seat.latest_serial()))
            .filter(|(_, serial)| *serial != 0)
            .max_by_key(|(_, serial)| *serial)
    }

    /// The loop-thread side of a request (the calloop channel callback).
    pub(crate) fn handle_clipboard_request(&mut self, request: ClipboardRequest) {
        match request {
            ClipboardRequest::Copy { selection, ack } => {
                let outcome = self.clipboard_copy(selection);
                // A caller that asked for the verdict gets it; one that could not
                // wait for it (a copy from this thread) is already gone.
                if let Some(ack) = ack {
                    let _ = ack.send(outcome);
                }
            },
            ClipboardRequest::Paste { selection, reply } => self.clipboard_paste(selection, reply),
        }
    }

    /// One copy, at the seat that holds this client's newest input serial. The
    /// sequence and the gate are [`SelectionPort::copy`]; what is left here is the
    /// seat pick and what a refusal costs — the own slot, which must not keep
    /// answering with text the compositor never took.
    fn clipboard_copy(&mut self, selection: WaylandSelection) -> CopyOutcome {
        let qh: QueueHandle<Self> = self.queue_handle.clone();
        let Some((seat, serial)) = self.clipboard_seat() else {
            warn!("wayland clipboard: no seat has given this client an input serial yet — {selection:?} copy not offered");
            self.clipboard_disown(selection);
            return CopyOutcome::Refused(CopyRefusal::NoSerial);
        };
        let outcome = WinitSelectionPort { state: self, seat, qh }.copy(selection, serial);
        if let CopyOutcome::Refused(refusal) = outcome {
            match refusal {
                CopyRefusal::NoDevice => {
                    warn!("wayland clipboard: the compositor binds no device for {selection:?} — copy refused");
                },
                CopyRefusal::Unfocused => {
                    // A compositor hands a selection only to the client its keyboard
                    // is on, and we hold no source for this one to keep instead.
                    warn!("wayland clipboard: no keyboard focus and no {selection:?} of ours — copy refused");
                },
                // Already said above, where the seat pick failed.
                CopyRefusal::NoSerial => {},
            }
            // Nothing of ours is on the selection, so the own slot must stop
            // answering as if something were: a paste then reads what the system
            // clipboard actually holds instead of the text we failed to place.
            self.clipboard_disown(selection);
        }
        outcome
    }

    /// Stop answering for `selection`: give up the source we offer (if any) and
    /// empty the own slot. The two belong together — the old code emptied the slot
    /// while still owning the selection, which left our source serving nothing.
    fn clipboard_disown(&mut self, selection: WaylandSelection) {
        match selection {
            WaylandSelection::Clipboard => drop(self.clipboard.clipboard_source.take()),
            WaylandSelection::Primary => drop(self.clipboard.primary_source.take()),
        }
        if let Ok(mut shared) = self.clipboard.shared.lock() {
            *shared.slot(selection) = None;
        }
    }

    fn clipboard_paste(&mut self, selection: WaylandSelection, reply: mpsc::Sender<Option<String>>) {
        // Own it? Answer at once (the handle already tried this slot, but the
        // ownership may have arrived since).
        if let Ok(mut shared) = self.clipboard.shared.lock() {
            if let Some(text) = shared.slot(selection).clone() {
                let _ = reply.send(Some(text));
                return;
            }
        }
        let pipe: Option<ReadPipe> = match selection {
            WaylandSelection::Clipboard => self.seats.values().find_map(|seat| {
                let offer = seat.data_device.as_ref()?.data().selection_offer()?;
                let mime = offer.with_mime_types(pick_text_mime)?;
                offer.receive(mime).ok()
            }),
            WaylandSelection::Primary => self.seats.values().find_map(|seat| {
                let offer = seat.primary_device.as_ref()?.data().selection_offer()?;
                let mime = offer.with_mime_types(pick_text_mime)?;
                offer.receive(mime).ok()
            }),
        };
        match pipe {
            Some(pipe) => read_on_thread(pipe, reply),
            None => {
                let _ = reply.send(None);
            },
        }
    }

    /// `wl_data_source.send` for a source of ours: serve the CLIPBOARD text.
    pub(crate) fn clipboard_send_request(&mut self, source: &WlDataSource, fd: WritePipe) -> bool {
        if !self.clipboard.clipboard_source.as_ref().is_some_and(|ours| ours.inner() == source) {
            return false;
        }
        let text = self
            .clipboard
            .shared
            .lock()
            .ok()
            .and_then(|shared| shared.clipboard.clone())
            .unwrap_or_default();
        write_on_thread(fd, text.into_bytes());
        true
    }

    /// `wl_data_source.cancelled` for a source of ours: a foreign client took
    /// the CLIPBOARD (or ours was replaced). Only the CURRENT source disowns.
    pub(crate) fn clipboard_cancelled(&mut self, source: &WlDataSource) -> bool {
        let ours = self.clipboard.clipboard_source.as_ref().is_some_and(|ours| ours.inner() == source);
        if ours {
            self.clipboard_disown(WaylandSelection::Clipboard);
        } else {
            source.destroy();
        }
        ours
    }
}

impl PrimarySelectionDeviceHandler for WinitState {
    fn selection(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _device: &ZwpPrimarySelectionDeviceV1,
    ) {
        // sctk stores the offer on the device's data; a paste reads it on demand.
    }
}

impl PrimarySelectionSourceHandler for WinitState {
    fn send_request(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        source: &ZwpPrimarySelectionSourceV1,
        _mime: String,
        write_pipe: WritePipe,
    ) {
        if !self.clipboard.primary_source.as_ref().is_some_and(|ours| ours.inner() == source) {
            return;
        }
        let text = self
            .clipboard
            .shared
            .lock()
            .ok()
            .and_then(|shared| shared.primary.clone())
            .unwrap_or_default();
        write_on_thread(write_pipe, text.into_bytes());
    }

    fn cancelled(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        source: &ZwpPrimarySelectionSourceV1,
    ) {
        if self.clipboard.primary_source.as_ref().is_some_and(|ours| ours.inner() == source) {
            self.clipboard_disown(WaylandSelection::Primary);
        } else {
            source.destroy();
        }
    }
}

sctk::delegate_primary_selection!(WinitState);

#[cfg(test)]
mod tests {
    use super::*;

    /// One seat, the compositor behind it, and the clipboard a SEPARATE client
    /// reads — the measured rules and nothing else.
    ///
    /// It is a [`SelectionPort`], so what these tests drive is the production
    /// [`SelectionPort::copy`]: the test supplies only the four primitives and the
    /// compositor they talk to. [`Self::wire`] is what this client sent, in order —
    /// the sequence a wire log of the real run would show.
    struct FakeSeat {
        /// The shared own-selection slot: what a source of OURS serves when a paste
        /// asks for it. [`WaylandClipboard::copy`] writes it before the loop runs,
        /// so it already holds the text of the copy in flight.
        slot: String,
        /// The source we offer, if we offer one.
        offered: Option<u32>,
        /// How many sources have been minted; the next one's id.
        minted: u32,
        /// Whether the seat's keyboard is on one of our surfaces.
        focused: bool,
        /// Whether the compositor binds a device for this selection at all.
        device: bool,
        /// The selection: which of OUR sources holds it, and the serial behind it.
        selection: Option<(u32, u32)>,
        /// What the compositor answers with once no source of ours holds the
        /// selection. It reads the bytes as soon as it ACCEPTS a claim (so it can
        /// keep serving them when the owner goes away), which is why a client that
        /// drops its source walks the clipboard back to the text before it.
        cached: String,
        /// Every protocol step this client sent, in order.
        wire: Vec<Step>,
    }

    /// The two requests a copy can send.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Step {
        /// `wl_data_source.destroy` — sctk sends it from the source's `Drop`.
        Destroy(u32),
        /// `wl_data_device.set_selection(source, serial)`.
        SetSelection { source: u32, serial: u32 },
    }

    impl FakeSeat {
        /// A focused client, on a compositor with a device, owning nothing yet.
        fn new() -> Self {
            Self {
                slot: String::new(),
                offered: None,
                minted: 0,
                focused: true,
                device: true,
                selection: None,
                cached: String::new(),
                wire: Vec::new(),
            }
        }

        /// One copy, the way the real one arrives: the handle writes the shared slot
        /// on the calling thread, then the loop runs the port.
        fn copy_text(&mut self, text: &str, serial: u32) -> CopyOutcome {
            self.slot = text.to_owned();
            self.copy(WaylandSelection::Clipboard, serial)
        }

        /// What another client on the system reads. A selection held by a source of
        /// ours is served LIVE out of the slot — `clipboard_send_request` reads the
        /// slot when the paste asks, not when the claim was made.
        fn read(&self) -> String {
            match self.selection {
                Some(_) => self.slot.clone(),
                None => self.cached.clone(),
            }
        }

        /// The proxy is destroyed. If that source WAS the selection, the compositor
        /// falls back to the bytes it cached when it accepted the claim.
        fn destroy(&mut self, source: u32) {
            self.wire.push(Step::Destroy(source));
            if self.selection.is_some_and(|(held, _)| held == source) {
                self.selection = None;
            }
        }
    }

    impl SelectionPort for FakeSeat {
        fn focused(&self) -> bool {
            self.focused
        }

        fn offers(&self, _selection: WaylandSelection) -> bool {
            self.offered.is_some()
        }

        fn release(&mut self, _selection: WaylandSelection) {
            if let Some(source) = self.offered.take() {
                self.destroy(source);
            }
        }

        fn claim(&mut self, _selection: WaylandSelection, serial: u32) -> bool {
            if !self.device {
                return false;
            }
            self.minted += 1;
            let source = self.minted;
            self.wire.push(Step::SetSelection { source, serial });
            // The compositor's verdict, by the two measured rules — and NOT readable
            // at the call, which is the whole reason the order has to be right.
            let dropped = !self.focused
                || self.selection.is_some_and(|(_, behind)| serial <= behind);
            if !dropped {
                self.selection = Some((source, serial));
                self.cached = self.slot.clone();
            }
            // Installing the new source destroys the one it displaces (sctk's `Drop`).
            if let Some(displaced) = self.offered.replace(source) {
                self.destroy(displaced);
            }
            true
        }
    }

    const FOUR: [&str; 4] = ["ZULU-00-aaaa", "ZULU-01-bbbb", "ZULU-02-cccc", "ZULU-03-dddd"];

    #[test]
    fn a_copy_with_no_fresh_input_behind_it_still_reaches_the_system_clipboard() {
        // Four copies re-spending ONE serial, which is what OSC 52, `aterm ctl copy`
        // and copy-on-select all produce. Glass: 4 of 4 (m17-tower, 2026-09-16).
        let mut seat = FakeSeat::new();
        for text in FOUR {
            assert!(seat.copy_text(text, 4426).placed(), "{text} was refused");
            assert_eq!(seat.read(), text, "the system clipboard did not take {text}");
        }
    }

    #[test]
    fn the_copy_a_focused_client_makes_releases_before_it_claims() {
        // The wire, not the constant: the second copy destroys the standing source
        // BEFORE it hands the compositor the new one.
        let mut seat = FakeSeat::new();
        seat.copy_text(FOUR[0], 4426);
        seat.copy_text(FOUR[1], 4426);
        assert_eq!(seat.wire, [
            Step::SetSelection { source: 1, serial: 4426 },
            Step::Destroy(1),
            Step::SetSelection { source: 2, serial: 4426 },
        ]);
        assert_eq!(CLAIM_ORDER, [ClaimStep::Release, ClaimStep::Claim]);
    }

    #[test]
    fn claiming_before_releasing_loses_every_other_copy_and_leaves_the_text_before_it() {
        // NOT the code's order — the order it USED to have, driven through the same
        // two primitives so the compositor above is known to be SHARP rather than
        // vacuously green. The old body claimed and let the `Option` assignment
        // destroy whatever it displaced, which is `claim` alone and nothing else.
        //
        // Glass, the same four copies on the unfixed binary (m17-tower, 2026-09-16):
        //   ZULU-00 LANDED / ZULU-01 STALE(clip=ZULU-00) / ZULU-02 LANDED /
        //   ZULU-03 STALE(clip=ZULU-02) — strictly alternating, each miss showing
        // the text of the copy before it.
        let mut seat = FakeSeat::new();
        let read_back: Vec<String> = FOUR
            .iter()
            .map(|text| {
                seat.slot = (*text).to_owned();
                seat.claim(WaylandSelection::Clipboard, 4426);
                seat.read()
            })
            .collect();
        assert_eq!(read_back, ["ZULU-00-aaaa", "ZULU-00-aaaa", "ZULU-02-cccc", "ZULU-02-cccc"]);
    }

    #[test]
    fn an_unfocused_copy_keeps_the_selection_it_holds_instead_of_walking_the_clipboard_backwards() {
        // The measured input, verbatim: two copies while the window is focused, the
        // keyboard then goes to another window, and the shell yanks again — OSC 52
        // or `aterm ctl copy`, neither of which needs this window to be focused.
        let mut seat = FakeSeat::new();
        seat.copy_text("FIX-01-aaaa", 4426);
        seat.copy_text("FIX-02-bbbb", 4426);
        seat.focused = false;
        let sent = seat.wire.len();
        let outcome = seat.copy_text("FIX-03-cccc", 4426);
        assert_eq!(seat.read(), "FIX-03-cccc", "the clipboard went back to the previous text");
        assert_eq!(outcome, CopyOutcome::Kept);
        assert_eq!(seat.wire.len(), sent, "an unfocused copy must send nothing at all");
    }

    #[test]
    fn an_unfocused_copy_that_holds_no_selection_reports_the_refusal_rather_than_success() {
        // Measured with a third-party X11 client holding the clipboard: the claim is
        // dropped whether or not we own anything, so the honest answer is a refusal —
        // `aterm ctl copy` then says `ERR pbcopy failed` instead of `OK <bytes>`.
        let mut seat = FakeSeat::new();
        seat.focused = false;
        seat.cached = "THIRD-PARTY".to_owned();
        let outcome = seat.copy_text("FIX-04-dddd", 4426);
        assert_eq!(seat.read(), "THIRD-PARTY", "the clipboard did not move, whatever we answered");
        assert!(!outcome.placed(), "a copy the compositor drops was reported as success");
        assert_eq!(outcome, CopyOutcome::Refused(CopyRefusal::Unfocused));
        assert!(seat.wire.is_empty(), "a claim the compositor drops is not worth a source");
    }

    #[test]
    fn a_selection_the_compositor_binds_no_device_for_is_refused_and_placed_says_so() {
        let mut seat = FakeSeat::new();
        seat.device = false;
        assert_eq!(
            seat.copy_text("ZULU-00-aaaa", 4426),
            CopyOutcome::Refused(CopyRefusal::NoDevice)
        );
        assert!(CopyOutcome::Claimed.placed());
        assert!(CopyOutcome::Kept.placed());
        assert!(!CopyOutcome::Refused(CopyRefusal::NoSerial).placed());
    }

    #[test]
    fn a_copy_off_the_loop_thread_answers_the_loop_s_verdict_and_never_guesses() {
        let (ack, verdict) = mpsc::channel();
        ack.send(CopyOutcome::Refused(CopyRefusal::Unfocused)).unwrap();
        assert!(!copy_verdict(&verdict));
        ack.send(CopyOutcome::Kept).unwrap();
        assert!(copy_verdict(&verdict));
        // A loop that died holding the request answers nothing, and nothing is not
        // success — the answer this whole path exists to stop giving.
        drop(ack);
        assert!(!copy_verdict(&verdict));
    }

    #[test]
    fn a_copy_with_no_loop_left_to_answer_it_is_false_and_leaves_no_own_slot_behind() {
        let (sender, receiver) = sctk::reexports::calloop::channel::channel();
        let shared = Arc::new(Mutex::new(Shared::default()));
        // A finished thread's id: never this one, so the copy takes the OFF-loop
        // path and would wait for a verdict if the request had been accepted.
        let loop_thread = std::thread::spawn(|| std::thread::current().id()).join().unwrap();
        let handle = WaylandClipboard::new(sender, Arc::clone(&shared), loop_thread);
        drop(receiver);
        assert!(!handle.copy(WaylandSelection::Clipboard, "hello"));
        assert_eq!(handle.paste_owned(WaylandSelection::Clipboard), None);
    }

    #[test]
    fn a_paste_prefers_utf8_then_the_legacy_text_types() {
        let s = |v: &[&str]| v.iter().map(|m| (*m).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            pick_text_mime(&s(&["image/png", "STRING", "text/plain;charset=utf-8"])).as_deref(),
            Some("text/plain;charset=utf-8")
        );
        assert_eq!(pick_text_mime(&s(&["TEXT", "STRING"])).as_deref(), Some("TEXT"));
        assert_eq!(pick_text_mime(&s(&["image/png"])), None);
        assert_eq!(pick_text_mime(&[]), None);
    }

    #[test]
    fn the_own_slot_answers_synchronously_and_a_foreign_slot_answers_none() {
        let (sender, _receiver) = sctk::reexports::calloop::channel::channel();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let handle = WaylandClipboard::new(sender, Arc::clone(&shared), std::thread::current().id());
        assert_eq!(handle.paste_owned(WaylandSelection::Clipboard), None);
        // THIS thread is the loop thread, so the copy must not wait for a verdict
        // only this thread could deliver.
        let began = Instant::now();
        assert!(handle.copy(WaylandSelection::Clipboard, "hello"));
        assert!(began.elapsed() < COPY_TIMEOUT, "a copy on the loop thread waited on the loop");
        assert_eq!(handle.paste_owned(WaylandSelection::Clipboard).as_deref(), Some("hello"));
        assert_eq!(handle.paste_owned(WaylandSelection::Primary), None);
        // On the loop thread a foreign read must not wait on the loop.
        assert_eq!(handle.paste(WaylandSelection::Primary), None);
        // A disown (a foreign client took it) empties the slot.
        shared.lock().unwrap().clipboard = None;
        assert_eq!(handle.paste_owned(WaylandSelection::Clipboard), None);
    }
}
