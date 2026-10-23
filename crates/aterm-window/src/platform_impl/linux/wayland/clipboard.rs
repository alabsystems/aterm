// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! Added by the aterm project in 2026; see the repository NOTICE.
//!
//! Wayland `CLIPBOARD` and `PRIMARY` selections ([`WaylandClipboard`], [`SelectionPort::copy`],
//! [`CLAIM_ORDER`], [`CopyOutcome`]).

use std::ffi::{CStr, CString};
use std::io::{PipeReader, Read, Write};
use std::os::fd::OwnedFd;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use aterm_wayland::selection::{Source, SourceEvent};
use tracing::{debug, warn};

use super::seat::data_device::set_nonblocking;
use super::state::WinitState;
use crate::platform_impl::common::poll::Waker;

/// Which selection a request addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WaylandSelection {
    /// The CLIPBOARD: explicit copy / paste.
    Clipboard,
    /// The PRIMARY selection: select-to-copy / middle-click paste.
    Primary,
}

/// The text mime types a copy offers, and the order a paste prefers them in.
const TEXT_MIMES: [&str; 5] =
    ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "TEXT", "STRING"];

/// [`TEXT_MIMES`] as a source offers them.
const TEXT_MIMES_C: [&CStr; 5] =
    [c"text/plain;charset=utf-8", c"UTF8_STRING", c"text/plain", c"TEXT", c"STRING"];

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

/// The order, as data rather than as prose.
pub(crate) const CLAIM_ORDER: [ClaimStep; 2] = [ClaimStep::Release, ClaimStep::Claim];

/// What a copy reached at the compositor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyOutcome {
    /// A fresh source is the selection.
    Claimed,
    /// The compositor would not take a fresh claim.
    Kept,
    /// Nothing of ours is on the selection.
    Refused(CopyRefusal),
}

/// Why a copy placed nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyRefusal {
    /// No seat has ever given this client an input serial.
    NoSerial,
    /// The compositor binds no device for this selection.
    NoDevice,
    /// The seat's keyboard is on another client's surface.
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
    /// `ack` is `Some` for a caller that waits for the verdict.
    Copy {
        selection: WaylandSelection,
        ack: Option<mpsc::Sender<CopyOutcome>>,
    },
    Paste {
        selection: WaylandSelection,
        reply: mpsc::Sender<Option<String>>,
    },
}

/// The own-selection slots, shared between the handle (any thread) and the loop.
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
    clipboard_source: Option<Offered>,
    primary_source: Option<Offered>,
    /// How many sources this client has made.
    sources_made: u64,
}

/// A source this client offers, with the number the clipboard gave it when it was made.
pub(crate) struct Offered {
    number: u64,
    /// Dropping it destroys it, which is the release.
    _source: Source,
}

impl ClipboardState {
    pub(crate) fn shared(&self) -> Arc<Mutex<Shared>> {
        Arc::clone(&self.shared)
    }

    /// The source this client offers for `selection`.
    fn offered(&self, selection: WaylandSelection) -> Option<&Offered> {
        match selection {
            WaylandSelection::Clipboard => self.clipboard_source.as_ref(),
            WaylandSelection::Primary => self.primary_source.as_ref(),
        }
    }

    fn offered_mut(&mut self, selection: WaylandSelection) -> &mut Option<Offered> {
        match selection {
            WaylandSelection::Clipboard => &mut self.clipboard_source,
            WaylandSelection::Primary => &mut self.primary_source,
        }
    }
}

/// The public handle: clone it anywhere, call it from any thread.
#[derive(Clone)]
pub struct WaylandClipboard {
    sender: mpsc::Sender<ClipboardRequest>,
    /// Wakes the loop for a request it should answer now.
    waker: Waker,
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
        sender: mpsc::Sender<ClipboardRequest>,
        waker: Waker,
        shared: Arc<Mutex<Shared>>,
        loop_thread: ThreadId,
    ) -> Self {
        Self { sender, waker, shared, loop_thread }
    }

    fn send(&self, request: ClipboardRequest) -> bool {
        let sent = self.sender.send(request).is_ok();
        if sent {
            self.waker.wake();
        }
        sent
    }

    /// Place `text` on `selection`.
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

    /// The text of `selection` when this client owns it.
    pub fn paste_owned(&self, selection: WaylandSelection) -> Option<String> {
        self.shared
            .lock()
            .ok()
            .and_then(|mut shared| shared.slot(selection).clone())
            .filter(|text| !text.is_empty())
    }

    /// The text of `selection`, ours or a foreign offer's.
    pub fn paste(&self, selection: WaylandSelection) -> Option<String> {
        if let Some(own) = self.paste_owned(selection) {
            return Some(own);
        }
        if std::thread::current().id() == self.loop_thread {
            debug!(
                "wayland clipboard: a foreign {selection:?} read on the loop thread answers None"
            );
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

/// Write `bytes` to a requester's pipe on a helper thread. Refuse a pipe we
/// cannot make non-blocking, so a stalled reader cannot park the worker in a write.
fn write_on_thread(pipe: OwnedFd, bytes: Vec<u8>) {
    if set_nonblocking(&pipe).is_err() {
        return;
    }
    let mut pipe = std::fs::File::from(pipe);
    let _ = std::thread::Builder::new().name("wayland-clipboard-send".into()).spawn(move || {
        let deadline = Instant::now() + PASTE_TIMEOUT;
        let mut at = 0;
        while at < bytes.len() {
            if Instant::now() >= deadline {
                break;
            }
            match pipe.write(&bytes[at..]) {
                Ok(0) => break,
                Ok(n) => at += n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
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
fn read_on_thread(mut pipe: PipeReader, reply: mpsc::Sender<Option<String>>) {
    if set_nonblocking(&pipe).is_err() {
        let _ = reply.send(None);
        return;
    }
    let _ = std::thread::Builder::new().name("wayland-clipboard-recv".into()).spawn(move || {
        let deadline = Instant::now() + PASTE_TIMEOUT;
        let mut bytes = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            if Instant::now() >= deadline {
                break;
            }
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
                    std::thread::sleep(Duration::from_millis(2));
                },
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {},
                Err(_) => break,
            }
        }
        let _ = reply.send(String::from_utf8(bytes).ok().filter(|s| !s.is_empty()));
    });
}

/// The MIME type a paste asks a foreign offer for.
pub(crate) fn pick_text_mime(advertised: &[String]) -> Option<String> {
    TEXT_MIMES.iter().find(|want| advertised.iter().any(|m| m == *want)).map(|m| (*m).to_owned())
}

/// The four things a copy does at one seat's selection, behind a seam.
pub(crate) trait SelectionPort {
    /// Whether this client holds the seat's keyboard focus.
    fn focused(&self) -> bool;

    /// Whether this client currently offers a source for `selection`.
    fn offers(&self, selection: WaylandSelection) -> bool;

    /// Give up the source we offer for `selection`; a no-op when we offer none.
    fn release(&mut self, selection: WaylandSelection);

    /// Offer a fresh source for `selection` and hand it to the seat with `serial`.
    fn claim(&mut self, selection: WaylandSelection, serial: u32) -> bool;

    /// THE COPY. One body, for production and for the test, so the order and the
    /// gate below are not things a call site can get wrong twice.
    fn copy(&mut self, selection: WaylandSelection, serial: u32) -> CopyOutcome {
        if !self.focused() {
            return if self.offers(selection) {
                CopyOutcome::Kept
            } else {
                CopyOutcome::Refused(CopyRefusal::Unfocused)
            };
        }
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
struct WinitSelectionPort<'a> {
    state: &'a mut WinitState,
    seat: u32,
}

impl SelectionPort for WinitSelectionPort<'_> {
    fn focused(&self) -> bool {
        self.state
            .seats
            .get(&self.seat)
            .is_some_and(super::seat::WinitSeatState::holds_keyboard_focus)
    }

    fn offers(&self, selection: WaylandSelection) -> bool {
        self.state.clipboard.offered(selection).is_some()
    }

    /// Dropping the source IS the release: aterm-wayland's `Source` destroys its
    /// proxy in `Drop` (as smithay-client-toolkit 0.19.2's `CopyPasteSource` and
    /// `PrimarySelectionSource` did before LW.7).
    fn release(&mut self, selection: WaylandSelection) {
        drop(self.state.clipboard.offered_mut(selection).take());
    }

    fn claim(&mut self, selection: WaylandSelection, serial: u32) -> bool {
        let number = self.state.clipboard.sources_made + 1;
        let seat = self.state.seats.get(&self.seat);
        let (manager, device) = match selection {
            WaylandSelection::Clipboard => (
                self.state.data_device_manager.as_ref(),
                seat.and_then(|seat| seat.data_device.as_ref()),
            ),
            WaylandSelection::Primary => (
                self.state.primary_selection_manager.as_ref(),
                seat.and_then(|seat| seat.primary_device.as_ref()),
            ),
        };
        let (Some(manager), Some(device)) = (manager, device) else {
            return false;
        };
        let requests = self.state.inbox.source_sink(selection, number);
        let source = match Source::new(manager, &TEXT_MIMES_C, 0, requests) {
            Ok(source) => source,
            Err(_error) => {
                warn!("wayland clipboard: no {selection:?} source: {_error}");
                return false;
            },
        };
        device.set_selection(Some(&source), serial);
        let clipboard = &mut self.state.clipboard;
        clipboard.sources_made = number;
        *clipboard.offered_mut(selection) = Some(Offered { number, _source: source });
        true
    }
}

impl WinitState {
    /// The seat a copy speaks for: the one whose keyboard or pointer last gave
    /// this client a serial — the compositor validates `set_selection` against it.
    fn clipboard_seat(&self) -> Option<(u32, u32)> {
        self.seats
            .keys()
            .map(|id| (*id, self.seats_book.latest_serial(*id).unwrap_or_default()))
            .filter(|(_, serial)| *serial != 0)
            .max_by_key(|(_, serial)| *serial)
    }

    /// The loop-thread side of a request (the loop's request channel).
    pub(crate) fn handle_clipboard_request(&mut self, request: ClipboardRequest) {
        match request {
            ClipboardRequest::Copy { selection, ack } => {
                let outcome = self.clipboard_copy(selection);
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
        let Some((seat, serial)) = self.clipboard_seat() else {
            warn!("wayland clipboard: no seat has given this client an input serial yet — {selection:?} copy not offered");
            self.clipboard_disown(selection);
            return CopyOutcome::Refused(CopyRefusal::NoSerial);
        };
        let outcome = WinitSelectionPort { state: self, seat }.copy(selection, serial);
        if let CopyOutcome::Refused(refusal) = outcome {
            match refusal {
                CopyRefusal::NoDevice => {
                    warn!("wayland clipboard: the compositor binds no device for {selection:?} — copy refused");
                },
                CopyRefusal::Unfocused => {
                    // is on, and we hold no source for this one to keep instead.
                    warn!("wayland clipboard: no keyboard focus and no {selection:?} of ours — copy refused");
                },
                CopyRefusal::NoSerial => {},
            }
            self.clipboard_disown(selection);
        }
        outcome
    }

    /// Stop answering for `selection`: give up the source we offer (if any) and
    /// empty the own slot.
    fn clipboard_disown(&mut self, selection: WaylandSelection) {
        drop(self.clipboard.offered_mut(selection).take());
        if let Ok(mut shared) = self.clipboard.shared.lock() {
            *shared.slot(selection) = None;
        }
    }

    fn clipboard_paste(
        &mut self,
        selection: WaylandSelection,
        reply: mpsc::Sender<Option<String>>,
    ) {
        // ownership may have arrived since).
        if let Ok(mut shared) = self.clipboard.shared.lock() {
            if let Some(text) = shared.slot(selection).clone() {
                let _ = reply.send(Some(text));
                return;
            }
        }
        let pipe: Option<PipeReader> = self.seats.values().find_map(|seat| {
            let device = match selection {
                WaylandSelection::Clipboard => seat.data_device.as_ref(),
                WaylandSelection::Primary => seat.primary_device.as_ref(),
            }?;
            let offer = device.selection()?;
            let mime = CString::new(pick_text_mime(&offer.mime_types)?).ok()?;
            device.receive_selection(&mime).ok()
        });
        match pipe {
            Some(pipe) => read_on_thread(pipe, reply),
            None => {
                let _ = reply.send(None);
            },
        }
    }

    /// A request to one of the sources this client offers, where the inbox has it.
    pub(crate) fn source_event(
        &mut self,
        selection: WaylandSelection,
        source: u64,
        event: SourceEvent,
    ) {
        if self.clipboard.offered(selection).map(|offered| offered.number) != Some(source) {
            return;
        }
        match event {
            SourceEvent::Send { fd, .. } => {
                let text = self
                    .clipboard
                    .shared
                    .lock()
                    .ok()
                    .and_then(|mut shared| shared.slot(selection).clone())
                    .unwrap_or_default();
                write_on_thread(fd, text.into_bytes());
            },
            SourceEvent::Cancelled => self.clipboard_disown(selection),
            _ => {},
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One seat, the compositor behind it, and the clipboard a SEPARATE client
    /// reads — the measured rules and nothing else.
    struct FakeSeat {
        /// The shared own-selection slot: what a source of OURS serves when a paste
        /// asks for it.
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
        /// selection.
        cached: String,
        /// Every protocol step this client sent, in order.
        wire: Vec<Step>,
    }

    /// The two requests a copy can send.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Step {
        /// `wl_data_source.destroy` — aterm-wayland's `Source` sends it from its `Drop`.
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

        /// The proxy is destroyed.
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
            let dropped =
                !self.focused || self.selection.is_some_and(|(_, behind)| serial <= behind);
            if !dropped {
                self.selection = Some((source, serial));
                self.cached = self.slot.clone();
            }
            if let Some(displaced) = self.offered.replace(source) {
                self.destroy(displaced);
            }
            true
        }
    }

    const FOUR: [&str; 4] = ["ZULU-00-aaaa", "ZULU-01-bbbb", "ZULU-02-cccc", "ZULU-03-dddd"];

    #[test]
    fn a_copy_with_no_fresh_input_behind_it_still_reaches_the_system_clipboard() {
        let mut seat = FakeSeat::new();
        for text in FOUR {
            assert!(seat.copy_text(text, 4426).placed(), "{text} was refused");
            assert_eq!(seat.read(), text, "the system clipboard did not take {text}");
        }
    }

    #[test]
    fn the_copy_a_focused_client_makes_releases_before_it_claims() {
        let mut seat = FakeSeat::new();
        seat.copy_text(FOUR[0], 4426);
        seat.copy_text(FOUR[1], 4426);
        assert_eq!(seat.wire, [
            Step::SetSelection { source: 1, serial: 4426 },
            Step::Destroy(1),
            Step::SetSelection { source: 2, serial: 4426 },
        ]);
    }

    #[test]
    fn claiming_before_releasing_loses_every_other_copy_and_leaves_the_text_before_it() {
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
        drop(ack);
        assert!(!copy_verdict(&verdict));
    }

    #[test]
    fn a_copy_with_no_loop_left_to_answer_it_is_false_and_leaves_no_own_slot_behind() {
        let (sender, receiver) = mpsc::channel();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let loop_thread = std::thread::spawn(|| std::thread::current().id()).join().unwrap();
        let waker = Waker::new().expect("eventfd");
        let handle = WaylandClipboard::new(sender, waker, Arc::clone(&shared), loop_thread);
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
        let (sender, _receiver) = mpsc::channel();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let waker = Waker::new().expect("eventfd");
        let handle =
            WaylandClipboard::new(sender, waker, Arc::clone(&shared), std::thread::current().id());
        assert_eq!(handle.paste_owned(WaylandSelection::Clipboard), None);
        assert!(handle.copy(WaylandSelection::Clipboard, "hello"));
        assert_eq!(handle.paste_owned(WaylandSelection::Clipboard).as_deref(), Some("hello"));
        assert_eq!(handle.paste_owned(WaylandSelection::Primary), None);
        assert_eq!(handle.paste(WaylandSelection::Primary), None);
        shared.lock().unwrap().clipboard = None;
        assert_eq!(handle.paste_owned(WaylandSelection::Clipboard), None);
    }
}
