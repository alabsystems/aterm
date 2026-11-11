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
use std::io::PipeReader;
use std::os::fd::OwnedFd;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;

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
        receiver
            .recv_timeout(crate::platform_impl::clipboard_transfer::TRANSFER_HANG)
            .ok()
            .flatten()
            .filter(|text| !text.is_empty())
    }
}

/// The loop's answer to one copy, as the caller's `bool`.
fn copy_verdict(verdict: &mpsc::Receiver<CopyOutcome>) -> bool {
    match crate::platform_impl::clipboard_reply::receive(verdict) {
        Ok(outcome) => outcome.placed(),
        Err(_) => false,
    }
}

/// Write `bytes` to a requester's pipe on a helper thread.
fn write_on_thread(pipe: OwnedFd, bytes: Vec<u8>) {
    if set_nonblocking(&pipe).is_err() {
        return;
    }
    let pipe = std::fs::File::from(pipe);
    let _ = std::thread::Builder::new().name("wayland-clipboard-send".into()).spawn(move || {
        let transfer = crate::platform_impl::clipboard_transfer::write(pipe, &bytes);
        if transfer.is_err() {
            debug!("wayland clipboard: selection transfer did not complete: {transfer:?}");
        }
    });
}

/// Read a foreign offer's pipe to EOF on a helper thread and answer `reply`.
fn read_on_thread(pipe: PipeReader, reply: mpsc::Sender<Option<String>>) {
    if set_nonblocking(&pipe).is_err() {
        let _ = reply.send(None);
        return;
    }
    let _ = std::thread::Builder::new().name("wayland-clipboard-recv".into()).spawn(move || {
        let _ = reply.send(crate::platform_impl::clipboard_transfer::read(pipe));
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

    /// Place the selection at the seat, releasing any existing source first; without keyboard
    /// focus, keep ours or refuse.
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

    /// Dropping the source destroys its proxy in `Drop`.
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

    /// One copy, at the seat that holds this client's newest input serial.
    fn clipboard_copy(&mut self, selection: WaylandSelection) -> CopyOutcome {
        let Some((seat, serial)) = self.clipboard_seat() else {
            warn!(
                "wayland clipboard: no seat has given this client an input serial yet — {selection:?} copy not offered"
            );
            self.clipboard_disown(selection);
            return CopyOutcome::Refused(CopyRefusal::NoSerial);
        };
        let outcome = WinitSelectionPort { state: self, seat }.copy(selection, serial);
        if let CopyOutcome::Refused(refusal) = outcome {
            match refusal {
                CopyRefusal::NoDevice => {
                    warn!(
                        "wayland clipboard: the compositor binds no device for {selection:?} — copy refused"
                    );
                },
                CopyRefusal::Unfocused => {
                    warn!(
                        "wayland clipboard: no keyboard focus and no {selection:?} of ours — copy refused"
                    );
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
        // Own it? Answer at once (the handle already tried this slot, but the
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
#[path = "clipboard_tests.rs"]
mod tests;
