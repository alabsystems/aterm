// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Control-socket lane scheduler: a request holds a thread, an idle connection
//! does not.
//!
//! - **Request lanes** (`CONTROL_WORKERS` fixed threads): authenticate and serve
//!   complete requests without waiting on the peer. Answers queue in [`Outbox`].
//! - **Wait lanes** ([`GrowPool`], up to `CONTROL_WAIT_WORKERS` threads): serve
//!   blocking/event-wait verbs (`await`, `ready`, `wait`, `turn`, bridge waits,
//!   bounded cross-session paste, remote relays). When wait lanes are full, at
//!   most `LaneLimits::inline` (`CONTROL_INLINE_WAITS`) request lanes serve waits;
//!   further waits get a busy reply while keeping the connection open.
//! - **Parker** ([`Parker`], one thread on Unix): multiplexes idle connections,
//!   partial requests/handshakes ([`Conn::carry`]), unsent answer tails ([`Outbox`]),
//!   and guarded-artifact challenge ACKs in one `poll(2)`.
//! - **Subscription lanes** ([`GrowPool`], up to `LaneLimits::subscriptions`
//!   threads): serve `subscribe` push streams until EOF.
//!
//! **Admission & eviction.** Placed connections are bounded by `LaneLimits::open`
//! (`control_connections_cap`). At the cap, fresh arrivals replace the stalest
//! parked connection that [`Conn::gives_way`] (authenticated and holding no live
//! supervisor/drive claim).
//!
//! **Parked memory budget.** Parked connections hold at most
//! [`PARKED_BYTES_PER_LANE`] per request lane; excess evicts the stalest byte
//! holder (sparing active readers via [`Conn::taking`] and claim holders until
//! last). Unsent answers and incoming requests of at least [`SPILL_FROM`] bytes
//! spill to private `mmap` regions ([`Spill`]) so their pages return to the OS
//! immediately on completion or close.
//!
//! Derived model: `aterm_models_native::control_lane_tenure_model`
//! (`control_lanes_conformance.rs`).

use std::collections::VecDeque;
use std::io::{BufRead, Read, Write};
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use aterm_uds::CtlStream;

use crate::budget::{Slot, Slots};

use super::{
    AUTH_READ_TIMEOUT, ArtifactAckOutcome, BoundedDispatch, ControlReply, MAX_REQUEST_LINE,
    PendingHandoff, Principal, ReplyWritten, Request, Scope, ServeDisposition, announced_body_of,
    read_authenticated_request_line, request_line_text,
};

/// What a lane serves a connection's requests with: the control server in the
/// window ([`super::ControlWorkerContext`]), a stub in the scheduling tests.
pub(super) trait LaneService: Send + Sync {
    /// Read the handshake and resolve its scope, with the verb a `TOKEN` line
    /// folded in. `None` closes the connection (the service has answered).
    fn authenticate(
        &self,
        stream: &CtlStream,
        reader: &mut LaneReader<'_>,
    ) -> Option<(Scope, Principal, Option<String>)>;
    /// Serve one authenticated request, queueing its answer in `out` (the
    /// lane delivers it, without waiting for the peer to read it). `reader`
    /// holds the whole request: its line was read from it, and the body a
    /// frame announces is in it. `None` keeps the connection.
    fn serve_line(
        &self,
        request: Request,
        scope: Scope,
        stream: &CtlStream,
        reader: &mut LaneReader<'_>,
        out: &mut Outbox,
    ) -> Option<ServeDisposition>;
    /// A frame's header has arrived, and not all of the body it announces: act
    /// on the header now, as the input boundary it is, while the parker waits
    /// for the body (the request is served whole once it has come, header and
    /// all). `false` refuses the frame, its answer in `out`: the connection
    /// closes once that is delivered. Nothing to act on by default.
    fn frame_header(&self, request: &Request, scope: Scope, out: &mut Outbox) -> bool {
        let _ = (request, scope, out);
        true
    }
    /// The reply `request` owes once it writes into a session's input, counted
    /// from before its dispatch until the lane delivers it
    /// (`control::InputReplies`): an update's Commit does not `_exit` over it.
    /// None by default.
    fn owed_input_reply(&self, request: &Request) -> Option<super::FinalReply> {
        let _ = request;
        None
    }
    /// Whether `request` belongs on a wait lane (see the module doc).
    fn is_wait(&self, request: &Request, scope: Scope) -> bool;
    /// Serve a connection that flipped to push until its stream ends, on the
    /// subscription lane that took it (see the module doc), with the request
    /// that flipped it.
    fn subscribe(&self, request: Request, scope: Scope, stream: CtlStream);
    /// Release every claim bound to connection `id`: it ended.
    fn release(&self, id: u64);
    /// Whether a live supervisor or drive lease still ends with connection
    /// `id`. The parker asks only for connections that acquired one before;
    /// that sticky cleanup flag does not say whether it still stands.
    #[cfg(unix)]
    fn has_live_claim(&self, id: u64) -> bool;
}

/// The caps. Production: `CONTROL_WORKERS`, `CONTROL_WAIT_WORKERS`,
/// `CONTROL_INLINE_WAITS`, `control_connections_cap()`,
/// `control_subscriptions_cap()`; the scheduling tests use small ones.
#[derive(Clone, Copy, Debug)]
pub(super) struct LaneLimits {
    /// Request lanes.
    pub(super) rpc: usize,
    /// Wait lanes, at most.
    pub(super) wait: usize,
    /// Max request lanes serving a wait when all wait lanes are busy.
    pub(super) inline: usize,
    /// Placed connections while the parker runs — its parking capacity.
    /// Pending admissions hold additional sockets (see the module doc).
    pub(super) open: usize,
    /// Subscription lanes, at most: push streams served at once, each holding
    /// its lane for its whole life and no open-connection slot.
    pub(super) subscriptions: usize,
}

/// A subscription's reply when no subscription lane can take it.
const SUBSCRIPTION_BUSY_LINE: &[u8] = b"ERR subscription capacity busy; retry\n";

/// How many times in a row the parker's loop may panic, with no pass completed
/// between them, before it stops (see the module doc). A panic that one
/// connection's wake raised never counts: that connection pays for it alone.
#[cfg(unix)]
const PARKER_PANICS_IN_A_ROW: usize = 3;

/// Window within which a peer that read answer bytes is treated as actively draining ([`Conn::taking`]).
#[cfg(unix)]
const TAKING_SPAN: Duration = Duration::from_secs(1);

/// The room a connection's read buffer ([`Unread`]) makes for the next read
/// from its peer once it is full, and so about the most one read takes.
const READ_BUFFER: usize = 8 * 1024;

/// Parked byte budget per request lane (`MAX_CONTROL_REPLY_LINE_BYTES`, 64 MiB across 8 lanes).
#[cfg(unix)]
const PARKED_BYTES_PER_LANE: usize = aterm_types::control_verbs::MAX_CONTROL_REPLY_LINE_BYTES;

/// An unsent answer, or a request still arriving, at least this long leaves the
/// heap while its connection is parked ([`Outbox::spill`],
/// [`Unread::room_for_the_request`]). A socket takes far less of an answer at
/// once (8 KiB on Darwin), so every answer this long parks owing most of it.
#[cfg(unix)]
const SPILL_FROM: usize = 16 << 10;

/// What a connection's unread bytes hold of the request at their head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Held {
    /// Not yet its line.
    Partial,
    /// A frame's header line (this many bytes, its newline not counted), and
    /// not yet all of the body it announces.
    Header(usize),
    /// A whole request — its line and the body it announces — or a line longer
    /// than any request may be, which the lane reads only to refuse.
    Whole,
}

/// What `buffered` holds of the request at its head, by the wire's grammar, so a lane and
/// the parker agree on when a request is whole. Before the handshake only its line
/// counts: a verb folded into it is judged when the lane unfolds it.
pub(super) fn held(buffered: &[u8], authenticated: bool) -> Held {
    let scan = &buffered[..buffered.len().min(MAX_REQUEST_LINE + 1)];
    let Some(end) = scan.iter().position(|byte| *byte == b'\n') else {
        return if buffered.len() > MAX_REQUEST_LINE {
            Held::Whole
        } else {
            Held::Partial
        };
    };
    if !authenticated {
        return Held::Whole;
    }
    let body = announced_body_of(&request_line_text(&buffered[..end]));
    if buffered.len() - (end + 1) >= body {
        Held::Whole
    } else {
        Held::Header(end)
    }
}

/// Whether `buffered` holds what a lane serves next: a whole request, or a
/// frame's header nobody has acted on yet.
fn servable(buffered: &[u8], authenticated: bool, header_done: bool) -> bool {
    match held(buffered, authenticated) {
        Held::Whole => true,
        Held::Header(_) => !header_done,
        Held::Partial => false,
    }
}

/// A connection's answer on its way to its peer, from the first byte the peer
/// has not taken yet, with what must last until it is out. A lane queues a
/// request's answer here and delivers what the peer takes at once; the parker
/// delivers the rest as the peer makes room. Nothing more is read from the
/// connection until it is all delivered — except, after a guarded artifact's
/// challenge, the one line that acknowledges it, which the parker awaits too.
#[derive(Default)]
pub(super) struct Outbox {
    /// The answer's bytes, in order: moved in as they were built, never copied
    /// into a buffer kept for the next answer.
    segments: VecDeque<Segment>,
    /// Bytes of the front segment the peer has taken.
    sent: usize,
    /// Close the connection once the answer is delivered: its request ended it.
    then_close: bool,
    /// Released once the last byte is out, or never will be: a seamless
    /// update's Commit waits on it ([`super::ReplyWire`]).
    written: Option<ReplyWritten>,
    /// An artifact answer's handoff ([`super::PendingHandoff`]): dropped with
    /// the outbox however the connection ends, failing closed.
    handoff: Option<PendingHandoff>,
    /// The answer is one the exit (or an update's Commit) waits for
    /// ([`super::FinalReplies`]): its request can end the process, or wrote
    /// into a session's input. Held until the answer is delivered, by the lane
    /// or by the parker, or the connection is gone.
    final_reply: Option<super::FinalReply>,
}

/// One run of an answer's bytes.
enum Segment {
    Bytes(Vec<u8>),
    /// A secret-bearing line (`askpass`), in its own guarded storage: moved
    /// here, never copied, and scrubbed as it drops.
    Secret(aterm_uds::sudo::WireBuffer),
    /// The unsent rest of a large answer, moved off the heap as its
    /// connection parked ([`Outbox::spill`]).
    #[cfg(unix)]
    Spilled(Spill),
}

impl Segment {
    fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Bytes(bytes) => bytes,
            Self::Secret(wire) => wire.as_bytes(),
            #[cfg(unix)]
            Self::Spilled(spill) => spill.as_slice(),
        }
    }
}

impl Outbox {
    /// Whether some of the answer is still the peer's to take.
    fn owed(&self) -> bool {
        !self.segments.is_empty()
    }

    /// Move the unsent tail of a large answer (`>= SPILL_FROM`) into an anonymous
    /// mapping ([`Spill`]) before parking so heap pages are freed on the serving
    /// lane and `munmap`ped on completion. Secret-bearing buffers are never spilled.
    #[cfg(unix)]
    fn spill(&mut self) {
        if !self
            .segments
            .iter()
            .all(|segment| matches!(segment, Segment::Bytes(_)))
        {
            return;
        }
        let unsent = self
            .segments
            .iter()
            .map(|segment| segment.as_bytes().len())
            .sum::<usize>()
            - self.sent;
        if unsent < SPILL_FROM {
            return;
        }
        let Some(mut spill) = Spill::zeroed(unsent) else {
            return;
        };
        let mut at = 0;
        for (index, segment) in self.segments.iter().enumerate() {
            let bytes = &segment.as_bytes()[if index == 0 { self.sent } else { 0 }..];
            spill.as_mut_slice()[at..at + bytes.len()].copy_from_slice(bytes);
            at += bytes.len();
        }
        self.segments.clear();
        self.segments.push_back(Segment::Spilled(spill));
        self.sent = 0;
    }

    /// The bytes it holds for its peer, as allocated or mapped.
    #[cfg(unix)]
    fn held(&self) -> usize {
        self.segments
            .iter()
            .map(|segment| match segment {
                Segment::Bytes(bytes) => bytes.capacity(),
                Segment::Secret(wire) => wire.as_bytes().len(),
                Segment::Spilled(spill) => spill.len(),
            })
            .sum()
    }

    /// Queue `bytes` after what is queued, copied: a short line.
    pub(super) fn extend_from_slice(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        match self.segments.back_mut() {
            Some(Segment::Bytes(tail)) => tail.extend_from_slice(bytes),
            _ => self.segments.push_back(Segment::Bytes(bytes.to_vec())),
        }
    }

    /// Queue `bytes` after what is queued, moved: an answer as it was built.
    pub(super) fn push_owned(&mut self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            self.segments.push_back(Segment::Bytes(bytes));
        }
    }

    /// Queue a secret-bearing line, moved into the outbox in its own storage.
    pub(super) fn push_secret(&mut self, wire: aterm_uds::sudo::WireBuffer) {
        if !wire.as_bytes().is_empty() {
            self.segments.push_back(Segment::Secret(wire));
        }
    }

    /// Queue a verb's answer, its body moved in, with the handoff and latch it
    /// carries ([`super::ControlReply::into_wire`]: a guarded artifact's
    /// identity is revalidated before any byte is queued, and its challenge
    /// follows the body). Its handoff fails at `ARTIFACT_HANDOFF_TIMEOUT`.
    pub(super) fn attach(&mut self, reply: ControlReply) {
        self.attach_within(reply, super::ARTIFACT_HANDOFF_TIMEOUT);
    }

    /// [`Self::attach`] with the handoff failing `handoff_timeout` from now.
    pub(super) fn attach_within(&mut self, reply: ControlReply, handoff_timeout: Duration) {
        debug_assert!(
            self.handoff.is_none() && self.written.is_none(),
            "a connection holds one answer at a time"
        );
        let (bytes, handoff, written) = reply.into_wire(handoff_timeout);
        self.push_owned(bytes);
        self.handoff = handoff;
        self.written = written;
        if !self.owed() {
            self.drained();
        }
    }

    /// When the answer's handoff fails if it is not over by then.
    fn deadline(&self) -> Option<Instant> {
        self.handoff.as_ref().map(PendingHandoff::deadline)
    }

    /// Whether the answer is out and the line that acknowledges its challenge
    /// is still owed.
    fn awaiting_ack(&self) -> bool {
        matches!(&self.handoff, Some(PendingHandoff::Acknowledged(ack)) if ack.on_wire)
    }

    /// Whether the connection closes once this answer is over (the bridge
    /// asks: [`super::serve_preresolved`]).
    #[cfg(unix)]
    pub(super) fn closes(&self) -> bool {
        self.then_close
    }

    /// What the bytes the peer `sent` after the challenge say
    /// ([`super::ack_verdict`]); `None` when no acknowledgement is owed.
    fn ack_verdict(&self, sent: &[u8], ended: bool) -> Option<bool> {
        match &self.handoff {
            Some(PendingHandoff::Acknowledged(ack)) if ack.on_wire => {
                super::ack_verdict(sent, &ack.nonce, ended)
            }
            _ => None,
        }
    }

    /// The acknowledgement's verdict: a matching line releases the guard (the
    /// write half shut first), anything else quarantines it. Either way the
    /// connection closes: a guarded reply is its connection's last.
    pub(super) fn settle(&mut self, acknowledged: bool, stream: &CtlStream) -> ArtifactAckOutcome {
        self.then_close = true;
        match self.handoff.take() {
            Some(PendingHandoff::Acknowledged(ack)) if acknowledged => {
                ack.acknowledged(stream);
                ArtifactAckOutcome::PeerAcknowledged
            }
            handoff => {
                drop(handoff);
                ArtifactAckOutcome::AcknowledgementQuarantined
            }
        }
    }

    /// Deliver what the peer takes: `Ok(true)` once all of it is out,
    /// `Ok(false)` when a non-blocking socket has no room left. A blocking
    /// socket that reports no room (a write timeout) fails, except within a
    /// handoff's deadline, which bounds the whole write instead.
    pub(super) fn deliver(
        &mut self,
        stream: &CtlStream,
        nonblocking: bool,
    ) -> std::io::Result<bool> {
        let mut writer = stream;
        self.deliver_to(&mut writer, nonblocking, |timeout| {
            stream.set_write_timeout(timeout)
        })
    }

    /// [`Self::deliver`] into `writer`, with `arm` setting its write timeout.
    /// A handoff written waiting is bound by its deadline, re-armed before
    /// every write so a peer taking a byte at a time buys nothing, and cleared
    /// once the answer is out (a write-only answer keeps its connection). A
    /// failure gives up the answer: its handoff drops (a guard to quarantine)
    /// and its latch is released.
    pub(super) fn deliver_to(
        &mut self,
        writer: &mut impl Write,
        nonblocking: bool,
        mut arm: impl FnMut(Option<Duration>) -> std::io::Result<()>,
    ) -> std::io::Result<bool> {
        let deadline = if nonblocking { None } else { self.deadline() };
        match self.write_out(writer, nonblocking, deadline, &mut arm) {
            Ok(false) => Ok(false),
            Ok(true) => {
                self.drained();
                if deadline.is_some() {
                    arm(None)?;
                }
                Ok(true)
            }
            Err(error) => {
                self.segments.clear();
                self.sent = 0;
                drop(self.written.take());
                drop(self.handoff.take());
                drop(self.final_reply.take());
                Err(error)
            }
        }
    }

    fn write_out(
        &mut self,
        writer: &mut impl Write,
        nonblocking: bool,
        deadline: Option<Instant>,
        arm: &mut impl FnMut(Option<Duration>) -> std::io::Result<()>,
    ) -> std::io::Result<bool> {
        let retry = |kind: std::io::ErrorKind| {
            kind == std::io::ErrorKind::Interrupted
                || (deadline.is_some()
                    && matches!(
                        kind,
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ))
        };
        while let Some(front) = self.segments.front() {
            if let Some(deadline) = deadline {
                arm(Some(super::remaining_artifact_io_budget(deadline)?))?;
            }
            let len = front.as_bytes().len();
            match writer.write(&front.as_bytes()[self.sent..]) {
                Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
                Ok(written) => {
                    self.sent += written;
                    if self.sent == len {
                        // A segment's memory goes as soon as it is out.
                        self.segments.pop_front();
                        self.sent = 0;
                    }
                }
                Err(error) if nonblocking && error.kind() == std::io::ErrorKind::WouldBlock => {
                    return Ok(false);
                }
                Err(error) if retry(error.kind()) => {}
                Err(error) => return Err(error),
            }
        }
        loop {
            if let Some(deadline) = deadline {
                arm(Some(super::remaining_artifact_io_budget(deadline)?))?;
            }
            match writer.flush() {
                Ok(()) => return Ok(true),
                Err(error) if retry(error.kind()) => {}
                Err(error) => return Err(error),
            }
        }
    }

    /// The last byte is out: the latch is released, the exit no longer waits
    /// for the answer, a write-only permit goes, and a guarded reply now awaits
    /// its acknowledgement.
    fn drained(&mut self) {
        drop(self.written.take());
        drop(self.final_reply.take());
        match self.handoff.take() {
            Some(PendingHandoff::Acknowledged(mut ack)) => {
                ack.mark_on_wire();
                self.handoff = Some(PendingHandoff::Acknowledged(ack));
            }
            Some(PendingHandoff::WriteOnly { .. }) | None => {}
        }
    }

    /// Deliver the answer, waiting, and see its handoff through: a guarded
    /// reply's acknowledgement is read from `reader` (what the connection
    /// already read past the request comes first, and fails it) under the
    /// handoff's deadline ([`super::read_ack_until`]). Where no parker can take
    /// the connection: a lane without one, and the bridge. `Ok(Some)` once an
    /// acknowledgement was judged; `Err` when the answer could not be written.
    pub(super) fn finish_waiting(
        &mut self,
        stream: &CtlStream,
        reader: &mut impl BufRead,
    ) -> std::io::Result<Option<ArtifactAckOutcome>> {
        if self.owed() {
            self.deliver(stream, false)?;
        }
        let Some(PendingHandoff::Acknowledged(ack)) = &self.handoff else {
            return Ok(None);
        };
        if !ack.on_wire {
            return Ok(None);
        }
        let acknowledged = super::read_ack_until(stream, reader, &ack.nonce, ack.deadline);
        Ok(Some(self.settle(acknowledged, stream)))
    }

    /// The nonce a guarded answer's challenge carries.
    #[cfg(test)]
    pub(super) fn nonce_for_test(&self) -> Option<String> {
        match &self.handoff {
            Some(PendingHandoff::Acknowledged(ack)) => Some(ack.nonce.clone()),
            _ => None,
        }
    }

    /// Whether the outbox still holds a handoff (a permit or a guard).
    #[cfg(test)]
    pub(super) fn holds_handoff_for_test(&self) -> bool {
        self.handoff.is_some()
    }
}

/// The frame handlers write their answers here as they would to a stream.
impl Write for Outbox {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Bytes a parked connection holds off the heap, in an anonymous mapping of
/// their own of a fixed length: a large answer's unsent rest
/// ([`Outbox::spill`]), or a large request while it arrives
/// ([`Unread::room_for_the_request`]). Unmapped on drop: its pages go back to
/// the system at once, and no byte of it is left anywhere in this process.
#[cfg(unix)]
struct Spill {
    base: std::ptr::NonNull<u8>,
    /// The mapping's length.
    len: usize,
}

// SAFETY: the mapping is owned by this value alone and reached only through it.
#[cfg(unix)]
unsafe impl Send for Spill {}

#[cfg(unix)]
impl Spill {
    /// A fresh mapping of `len` zeroed bytes, or `None` when none can be made
    /// (or `len` is 0).
    fn zeroed(len: usize) -> Option<Self> {
        if len == 0 {
            return None;
        }
        // SAFETY: a fresh private anonymous read-write mapping with no address
        // hint; the result is checked before any use.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return None;
        }
        Some(Self {
            base: std::ptr::NonNull::new(base.cast::<u8>())?,
            len,
        })
    }

    /// A fresh mapping of `len` bytes (no fewer than `bytes` holds) that
    /// starts with a copy of `bytes`.
    fn copy_of(bytes: &[u8], len: usize) -> Option<Self> {
        let mut spill = Self::zeroed(len.max(bytes.len()))?;
        spill.as_mut_slice()[..bytes.len()].copy_from_slice(bytes);
        Some(spill)
    }

    fn len(&self) -> usize {
        self.len
    }

    fn as_slice(&self) -> &[u8] {
        // SAFETY: the mapping's `len` bytes, all initialized (an anonymous
        // mapping starts zeroed), mapped for as long as `self` lives.
        unsafe { std::slice::from_raw_parts(self.base.as_ptr(), self.len) }
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as in `as_slice`, and borrowed exclusively through `self`.
        unsafe { std::slice::from_raw_parts_mut(self.base.as_ptr(), self.len) }
    }
}

#[cfg(unix)]
impl Drop for Spill {
    fn drop(&mut self) {
        // SAFETY: the mapping `zeroed` made, unmapped once, here; no borrow of
        // it outlives `self`.
        unsafe { libc::munmap(self.base.as_ptr().cast(), self.len) };
    }
}

/// What gathering a request from a peer, without waiting, came to (only a
/// parker's lanes gather: unix).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(unix), allow(dead_code))]
enum Gathered {
    /// The bytes hold what was asked for.
    Enough,
    /// Not yet, and nothing more is there to read now.
    Short,
    /// The peer sends no more (end of file).
    Ended,
    /// The connection failed.
    Broken,
}

/// Per-connection unread byte buffer shared across lanes and the parker.
/// Large requests (`>= SPILL_FROM`) gather in a [`Spill`] mapping.
#[derive(Default)]
pub(super) struct Unread {
    /// Initialized storage: `at..end` is unread, and `end..` is room for the
    /// next read, zeroed once when the storage grows rather than per read.
    bytes: Vec<u8>,
    /// The storage instead of `bytes` (empty meanwhile) while a large request
    /// is held off the heap, with room for the whole of it; unmapped once it
    /// is served, or if it outgrows its room.
    #[cfg(unix)]
    mapped: Option<Spill>,
    at: usize,
    end: usize,
}

impl Unread {
    /// A buffer that already holds `bytes`, as if read (the tests' carry).
    #[cfg(test)]
    pub(super) fn holding(bytes: Vec<u8>) -> Self {
        let end = bytes.len();
        Self {
            bytes,
            #[cfg(unix)]
            mapped: None,
            at: 0,
            end,
        }
    }

    /// What `at` and `end` index: the mapping while there is one, else
    /// `bytes`.
    fn storage(&self) -> &[u8] {
        #[cfg(unix)]
        if let Some(mapped) = &self.mapped {
            return mapped.as_slice();
        }
        &self.bytes
    }

    fn as_slice(&self) -> &[u8] {
        &self.storage()[self.at..self.end]
    }

    fn is_empty(&self) -> bool {
        self.at == self.end
    }

    /// `amount` bytes are served. An emptied buffer keeps its storage for the
    /// next read, unless a request larger than one read's room grew it (a
    /// mapping goes, its pages with it); a parked idle connection holds none
    /// ([`Self::hold_only_what_is_owed`]).
    fn consume(&mut self, amount: usize) {
        self.at = self.end.min(self.at + amount);
        if self.at == self.end {
            self.at = 0;
            self.end = 0;
            #[cfg(unix)]
            {
                self.mapped = None;
            }
            if self.bytes.capacity() > READ_BUFFER {
                self.bytes = Vec::new();
            }
        }
    }

    /// Room for the next read. Once little is left, what was served goes and
    /// the unread rest moves to the front; the storage grows only when full.
    /// A mapping gives its room after what it holds; one with none left goes
    /// back to the heap first.
    fn room(&mut self) -> &mut [u8] {
        #[cfg(unix)]
        if self
            .mapped
            .as_ref()
            .is_some_and(|mapped| mapped.len() == self.end)
        {
            self.move_to_heap();
        }
        #[cfg(unix)]
        if let Some(mapped) = self.mapped.as_mut() {
            return &mut mapped.as_mut_slice()[self.end..];
        }
        if self.at > 0 && self.bytes.len() - self.end < READ_BUFFER {
            self.bytes.copy_within(self.at..self.end, 0);
            self.end -= self.at;
            self.at = 0;
        }
        if self.end == self.bytes.len() {
            self.bytes.resize(self.end + READ_BUFFER, 0);
        }
        &mut self.bytes[self.end..]
    }

    /// One read from `stream` onto the end: `Ok(0)` at its end of file.
    fn read_from(&mut self, stream: &CtlStream) -> std::io::Result<usize> {
        let mut reader = stream;
        let read = reader.read(self.room())?;
        self.end += read;
        Ok(read)
    }

    /// Put `line` and its newline in front of what is unread.
    fn put_back_line(&mut self, line: &[u8]) {
        let mut bytes = Vec::with_capacity(line.len() + 1 + self.as_slice().len());
        bytes.extend_from_slice(line);
        bytes.push(b'\n');
        bytes.extend_from_slice(self.as_slice());
        self.at = 0;
        self.end = bytes.len();
        self.bytes = bytes;
        #[cfg(unix)]
        {
            self.mapped = None;
        }
    }

    /// What is unread, moved from a mapping back onto the heap.
    #[cfg(unix)]
    fn move_to_heap(&mut self) {
        if let Some(mapped) = self.mapped.take() {
            self.bytes = mapped.as_slice()[self.at..self.end].to_vec();
            self.at = 0;
            self.end = self.bytes.len();
        }
    }

    /// The bytes it holds, as allocated or mapped.
    #[cfg(unix)]
    fn held(&self) -> usize {
        self.bytes.capacity() + self.mapped.as_ref().map_or(0, Spill::len)
    }

    /// As its connection parks: no storage at all when nothing is unread (its
    /// next request is read into storage made then), and a large request a
    /// lane began to read off the heap ([`Self::room_for_the_request`], as the
    /// parker's own reads would).
    #[cfg(unix)]
    fn hold_only_what_is_owed(&mut self, authenticated: bool) {
        if self.is_empty() {
            *self = Self::default();
        } else {
            self.room_for_the_request(authenticated);
        }
    }

    /// Allocate a [`Spill`] mapping when an incoming frame or long line reaches
    /// [`SPILL_FROM`] so slow peers do not pin heap pages while parked.
    #[cfg(unix)]
    fn room_for_the_request(&mut self, authenticated: bool) {
        let carried = self.as_slice();
        let (need, large) = match held(carried, authenticated) {
            Held::Header(end) => {
                let need = end + 1 + announced_body_of(&request_line_text(&carried[..end]));
                (need, need >= SPILL_FROM)
            }
            Held::Partial => (
                MAX_REQUEST_LINE + 1,
                carried.len() + READ_BUFFER >= SPILL_FROM,
            ),
            Held::Whole => return,
        };
        let room = self
            .mapped
            .as_ref()
            .map_or(0, |mapped| mapped.len() - self.at);
        if need <= room || (self.mapped.is_none() && !large) {
            return;
        }
        let unread = carried.len();
        match Spill::copy_of(carried, need) {
            Some(mapped) => {
                self.bytes = Vec::new();
                self.mapped = Some(mapped);
                self.at = 0;
                self.end = unread;
            }
            None => self.move_to_heap(),
        }
    }

    /// Read what the peer has sent, until `enough(unread)` or nothing more is
    /// there. The socket must not block: a parked one does not, and a lane
    /// switches its own first ([`gather_here`]).
    #[cfg(unix)]
    fn gather(&mut self, stream: &CtlStream, enough: impl Fn(&[u8]) -> bool) -> Gathered {
        self.gather_with(stream, enough, |_| {})
    }

    /// [`Self::gather`], running `before_read` before each read.
    #[cfg(unix)]
    fn gather_with(
        &mut self,
        stream: &CtlStream,
        enough: impl Fn(&[u8]) -> bool,
        mut before_read: impl FnMut(&mut Self),
    ) -> Gathered {
        loop {
            if enough(self.as_slice()) {
                return Gathered::Enough;
            }
            before_read(self);
            match self.read_from(stream) {
                Ok(0) => return Gathered::Ended,
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    return Gathered::Short;
                }
                Err(_) => return Gathered::Broken,
            }
        }
    }
}

/// How a lane, and the request it serves, reads a connection: what its
/// [`Unread`] holds first — bytes an earlier lane or the parker already read —
/// then the stream, read into that same buffer. Held bytes, being in the
/// buffer and not in the socket, count wherever a lane asks whether anything is
/// buffered: a connection that has them never parks, and a readiness wait
/// never sleeps on the socket in front of them.
pub(super) struct LaneReader<'a> {
    stream: &'a CtlStream,
    unread: &'a mut Unread,
}

impl<'a> LaneReader<'a> {
    pub(super) fn new(stream: &'a CtlStream, unread: &'a mut Unread) -> Self {
        Self { stream, unread }
    }

    /// The connection's stream (a readiness wait polls it).
    pub(super) fn stream(&self) -> &'a CtlStream {
        self.stream
    }

    /// What is read and not yet consumed, with no syscall.
    pub(super) fn buffer(&self) -> &[u8] {
        self.unread.as_slice()
    }

    /// Every byte read and not yet consumed, taken out: a relay forwards them
    /// before its own reads. Never reads the stream, as `fill_buf` would on an
    /// empty buffer — the common case, a one-line forward whose client sent
    /// `TOKEN <hex> @<sid> <verb>` and awaits the reply — where the relay would
    /// then wait forever before it starts.
    pub(super) fn take_buffered(&mut self) -> Vec<u8> {
        let taken = self.buffer().to_vec();
        self.unread.consume(taken.len());
        taken
    }

    /// Put `line` back at the head of what is unread: the verb a `TOKEN`
    /// handshake folds in, served as the connection's first request.
    fn put_back_line(&mut self, line: &[u8]) {
        self.unread.put_back_line(line);
    }
}

impl Read for LaneReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        // A read at least as large as the buffer's room bypasses it when it
        // holds nothing (a frame's body read without a parker).
        if self.unread.is_empty() && buf.len() >= READ_BUFFER {
            let mut stream = self.stream;
            return stream.read(buf);
        }
        let read = {
            let available = self.fill_buf()?;
            let read = available.len().min(buf.len());
            buf[..read].copy_from_slice(&available[..read]);
            read
        };
        self.consume(read);
        Ok(read)
    }
}

impl BufRead for LaneReader<'_> {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        if self.unread.is_empty() {
            self.unread.read_from(self.stream)?;
        }
        Ok(self.unread.as_slice())
    }

    fn consume(&mut self, amount: usize) {
        self.unread.consume(amount);
    }
}

/// Cached `O_NONBLOCK` state of a connection descriptor (non-blocking while
/// parked or gathering/delivering on a lane; blocking during handler execution;
/// `None` when unknown). Uses `O_NONBLOCK` because Darwin ignores `MSG_DONTWAIT`
/// on `AF_UNIX` sockets.
#[derive(Clone, Copy, Debug, Default)]
struct SocketMode(Option<bool>);

impl SocketMode {
    /// Make `stream` non-blocking, or blocking, unless it is known to be.
    fn set(&mut self, stream: &CtlStream, nonblocking: bool) -> std::io::Result<()> {
        if self.0 == Some(nonblocking) {
            return Ok(());
        }
        // Off unix no parker runs and nothing stops blocking: a descriptor
        // keeps the mode it was accepted in.
        #[cfg(not(unix))]
        {
            let _ = stream;
            debug_assert!(!nonblocking, "only a parker's lanes stop blocking");
        }
        #[cfg(unix)]
        {
            self.0 = None;
            stream.set_nonblocking(nonblocking)?;
            #[cfg(test)]
            MODE_SWITCHES.with(|switches| switches.set(switches.get() + 1));
        }
        self.0 = Some(nonblocking);
        Ok(())
    }

    /// A handler has had the descriptor: its mode is no longer known.
    fn forget(&mut self) {
        self.0 = None;
    }
}

#[cfg(all(test, unix))]
thread_local! {
    /// The descriptor mode switches this thread has made ([`SocketMode::set`]).
    static MODE_SWITCHES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The descriptor mode switches the calling thread has made so far.
#[cfg(all(test, unix))]
pub(super) fn mode_switches_here() -> u64 {
    MODE_SWITCHES.with(std::cell::Cell::get)
}

/// A connection's claim to its own serial, and the release of whatever was
/// bound to it, as a drop guard: when the connection ends (closed, flipped to
/// push, dropped by a lane's unwind or by the parker) its claims go with it.
struct ConnTenure {
    id: u64,
    claimed: bool,
    service: Arc<dyn LaneService>,
}

impl Drop for ConnTenure {
    fn drop(&mut self) {
        if self.claimed {
            self.service.release(self.id);
        }
    }
}

/// One connection between placements.
pub(super) struct Conn {
    stream: CtlStream,
    /// `None` until the handshake on its first lane.
    scope: Option<Scope>,
    /// Authenticated principal from the handshake.
    principal: Principal,
    /// Typing caller identity cached on first typing verb ([`super::ServingCaller`]).
    caller: Option<Arc<crate::typed_by::Caller>>,
    tenure: ConnTenure,
    /// Bytes read from the peer and not served yet, read first wherever the
    /// connection lands ([`LaneReader`]): the requests a request lane had read
    /// past the wait it moved to a wait lane, or what has come of the next
    /// request while it is not whole (the parker adds to it as more arrives).
    carry: Unread,
    /// Its descriptor's `O_NONBLOCK`, as the lanes last set it.
    mode: SocketMode,
    /// The part of its last answer the peer has not taken yet, and what must
    /// last until it is out (or, for a guarded artifact, acknowledged).
    reply: Outbox,
    /// The frame header at the head of [`Self::carry`] has been acted on
    /// ([`LaneService::frame_header`]): only its whole body wakes it now.
    header_done: bool,
    /// Until the handshake: when the parker drops the connection if its first
    /// line is still not whole (`AUTH_READ_TIMEOUT` from admission). An
    /// artifact answer's handoff carries its own ([`Self::deadline`]).
    expires: Option<Instant>,
    /// When its peer last sent or took a byte or a lane last served it: of
    /// the parked connections, the one with the oldest is the STALEST, closed
    /// first when room is needed ([`stalest`]).
    #[cfg_attr(not(unix), allow(dead_code))]
    stirred: Instant,
    /// When its peer last took bytes of an answer it was owed while parked
    /// (the room it made woke the parker): within [`TAKING_SPAN`] of a byte
    /// budget pass, a peer TAKING its answer, closed for the budget only
    /// after every holder that is not ([`Lanes::keep_parked_bytes_within`]).
    #[cfg_attr(not(unix), allow(dead_code))]
    taking: Option<Instant>,
    /// Held from parking until a request lane takes it back.
    parked: Option<Slot>,
    /// Held from admission until the connection ends (or flips to push). Last,
    /// so a new peer is admitted only after this one's claims are released.
    _open: Slot,
}

impl Conn {
    /// When the parker drops the connection if it is still parked: its
    /// handshake's lifetime, or its answer's handoff deadline.
    #[cfg(unix)]
    fn deadline(&self) -> Option<Instant> {
        self.expires.or_else(|| self.reply.deadline())
    }

    /// Shut down both socket halves via [`aterm_uds::hangup::hang_up`], release
    /// connection claims, and free the open slot. Separate half-shutdowns ensure
    /// half-closed peers (`nc -U`) still observe EOF on Darwin even if a forked
    /// child inherited the fd.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ControlLaneTenure",
            action = "Close",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ControlLaneTenure",
            action = "WaitClose",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ControlLaneTenure",
            action = "InlineClose",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    fn close(self) {
        aterm_uds::hangup::hang_up(&self.stream);
    }

    /// A request lane takes a resumed connection back: its parking slot is
    /// free again. Its socket stays non-blocking, as parked: the lane switches
    /// it when what it does next needs the other mode ([`SocketMode`]).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ControlLaneTenure",
            action = "Pick",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    fn take_back(&mut self) {
        self.parked = None;
    }
}

/// The request lanes' work: a freshly accepted connection (holding the open
/// slot its admission took), or a parked one that turned readable (boxed, so
/// every queued job is the size of a stream, not of a connection's buffers).
enum Job {
    Fresh(CtlStream, Slot),
    #[cfg_attr(
        not(unix),
        expect(dead_code, reason = "only the unix parker resumes a connection")
    )]
    Resume(Box<Conn>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lane {
    Rpc,
    Wait,
}

/// Why a lane let go of a connection.
enum Here {
    Closed,
    Subscribe(Request, Scope),
    /// Between requests — idle, or waiting on its peer for the rest of its
    /// next request or to take the rest of its answer: park it.
    #[cfg_attr(not(unix), allow(dead_code))]
    Idle,
    /// A wait read on a request lane: move it, with what the lane had read past
    /// it in [`Conn::carry`].
    Wait(Request),
}

/// A connection that flipped to push, on its way to a subscription lane.
struct Push {
    stream: CtlStream,
    request: Request,
    scope: Scope,
}

/// A lane set grown one thread at a time up to `max`, and kept: the wait lanes
/// and the subscription lanes. A job goes to an idle thread, or to a new one
/// while the cap allows; it never queues behind running work, so a job no
/// thread can take at once is handed back.
struct GrowPool<J> {
    /// Its threads' name, each suffixed with its index.
    name: &'static str,
    max: usize,
    state: Mutex<GrowState<J>>,
    ready: Condvar,
}

struct GrowState<J> {
    /// Jobs for threads serving nothing: one is queued only while more threads
    /// serve nothing than jobs are queued.
    queue: VecDeque<J>,
    /// Threads started or starting.
    threads: usize,
    /// Threads serving a job.
    running: usize,
}

/// A job handed to the one thread started for it ([`GrowPool::submit`]),
/// which serves it first. A thread that never started never took it, so the
/// submitter takes it back from here.
struct Handed<J>(Arc<Mutex<Option<J>>>);

impl<J> Handed<J> {
    fn new(job: J) -> Self {
        Self(Arc::new(Mutex::new(Some(job))))
    }

    fn share(&self) -> Self {
        Self(Arc::clone(&self.0))
    }

    fn take(&self) -> Option<J> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

/// Run before each wait lane's spawn, with no lock held: the scheduling
/// tests' slow or failing spawn.
#[cfg(test)]
type WaitSpawnHook = Arc<dyn Fn() -> std::io::Result<()> + Send + Sync>;

impl<J> GrowPool<J> {
    fn new(name: &'static str, max: usize) -> Self {
        Self {
            name,
            max,
            state: Mutex::new(GrowState {
                queue: VecDeque::new(),
                threads: 0,
                running: 0,
            }),
            ready: Condvar::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, GrowState<J>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Start threads with nothing to serve yet, by `start`, until the pool has
    /// `count` (never more than `max`). Returns how many it has. Each start
    /// runs with no lock held, as [`Self::submit`]'s does.
    fn reserve(
        &self,
        count: usize,
        mut start: impl FnMut(String, Option<Handed<J>>) -> std::io::Result<()>,
    ) -> usize {
        loop {
            let mut state = self.lock();
            if state.threads >= count.min(self.max) {
                return state.threads;
            }
            let name = format!("{}-{}", self.name, state.threads);
            state.threads += 1;
            drop(state);
            if let Err(error) = start(name, None) {
                aterm_log::warn!("{} could not start: {error}", self.name);
                let mut state = self.lock();
                state.threads -= 1;
                return state.threads;
            }
        }
    }

    /// Dispatch `job` to an idle thread or spawn a new one (without holding the pool
    /// lock) up to `max`. Returns `Err(job)` at capacity or if thread spawn fails.
    fn submit(
        &self,
        job: J,
        start: impl FnOnce(String, Option<Handed<J>>) -> std::io::Result<()>,
    ) -> Result<(), J> {
        let mut state = self.lock();
        // Each thread serving nothing takes one queued job.
        if state.threads - state.running > state.queue.len() {
            state.queue.push_back(job);
            self.ready.notify_one();
            return Ok(());
        }
        if state.threads >= self.max {
            return Err(job);
        }
        let name = format!("{}-{}", self.name, state.threads);
        state.threads += 1;
        state.running += 1;
        drop(state);
        let handed = Handed::new(job);
        let Err(error) = start(name, Some(handed.share())) else {
            return Ok(());
        };
        aterm_log::warn!("{} could not start: {error}", self.name);
        let mut state = self.lock();
        state.threads -= 1;
        state.running -= 1;
        drop(state);
        Err(handed
            .take()
            .expect("only the thread started for a handed job takes it"))
    }

    /// One thread's life: the job handed to it when it was started for one,
    /// then each job it takes, a panic costing only that job.
    fn work(&self, first: Option<Handed<J>>, serve: impl Fn(J)) {
        if let Some(first) = first {
            // `submit` counted this thread running its job before it started.
            if let Some(job) = first.take() {
                self.serve_one(job, &serve);
            }
            self.lock().running -= 1;
        }
        let mut state = self.lock();
        loop {
            let Some(job) = state.queue.pop_front() else {
                state = self
                    .ready
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
                continue;
            };
            state.running += 1;
            drop(state);
            self.serve_one(job, &serve);
            state = self.lock();
            state.running -= 1;
        }
    }

    fn serve_one(&self, job: J, serve: &impl Fn(J)) {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| serve(job))).is_err() {
            let thread = std::thread::current();
            let name = thread.name().unwrap_or(self.name);
            aterm_log::warn!("{name} recovered after a connection panic");
        }
    }

    /// Its work (queued, or handed to a starting thread, plus running), its
    /// threads, and how many of them serve nothing. A thread that holds the
    /// lock and never lets go fails the census within the house hang
    /// detector, instead of hanging the test.
    #[cfg(test)]
    #[cfg_attr(not(unix), allow(dead_code))] // its tests are unix-only
    fn census(&self) -> (usize, usize, usize) {
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        let state = loop {
            match self.state.try_lock() {
                Ok(state) => break state,
                Err(std::sync::TryLockError::Poisoned(poisoned)) => break poisoned.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    assert!(
                        Instant::now() < deadline,
                        "the {} lock was never free for the census",
                        self.name
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        };
        (
            state.queue.len() + state.running,
            state.threads,
            state.threads - state.running,
        )
    }
}

#[cfg(unix)]
struct Parker {
    /// Connections parked now, at most `limits.open`.
    parked: Arc<Slots>,
    /// What was handed to the parker and not yet taken — `None` once it has
    /// STOPPED, which a lane parking a connection and admission at the cap
    /// read under this lock (so nothing is handed over after the stop's last
    /// look) and answer by keeping the connection, or turning it away.
    inbox: Mutex<Option<Inbox>>,
    /// What the parked connections held at the end of the parker's last pass,
    /// in bytes ([`Conn::held_bytes`]).
    held: AtomicUsize,
    /// Of the parked connections at the end of the parker's last pass, those
    /// that did not give way ([`Conn::gives_way`]): the tests' census.
    #[cfg(test)]
    spared: AtomicUsize,
    /// Held by a test to keep the parker from taking what it is handed
    /// ([`Lanes::parker_pause`]).
    #[cfg(test)]
    pause: Mutex<()>,
    /// Whether the parker still polls: `inbox.is_some()`, readable without the
    /// lock by admission and [`Lanes::parker_runs`].
    running: AtomicBool,
    /// The write end of the parker's bell (non-blocking): a byte wakes its poll.
    bell: std::os::unix::net::UnixStream,
    /// Panics to raise in the parker's loop, one per pass, before it looks at
    /// what woke it — the scheduling tests' injected failure.
    #[cfg(test)]
    faults: AtomicUsize,
}

/// What lanes and admission hand the parker, taken at its next pass.
#[cfg(unix)]
#[derive(Default)]
struct Inbox {
    /// Connections parked between requests.
    parked: Vec<Conn>,
    /// Fresh connections admitted at the cap, each to take the place of the
    /// stalest parked connection ([`Lanes::take_the_stalest_place`]).
    fresh: Vec<CtlStream>,
}

/// The whole lane set of one control socket.
pub(super) struct Lanes {
    limits: LaneLimits,
    rpc: BoundedDispatch<Job>,
    waits: GrowPool<(Conn, Request)>,
    subscriptions: GrowPool<Push>,
    /// Request lanes serving a wait right now (bounded by `limits.inline`).
    inline: Arc<Slots>,
    /// Connections open now: bounded by `limits.open` while a parker runs,
    /// and by the request lanes when none does.
    open: Arc<Slots>,
    #[cfg(unix)]
    parker: OnceLock<Arc<Parker>>,
    service: OnceLock<Arc<dyn LaneService>>,
    /// The scheduling tests' own [`super::FinalReplies`]
    /// ([`Self::count_final_replies_on`]); the instance's otherwise.
    #[cfg(all(test, unix))]
    final_replies: OnceLock<&'static super::FinalReplies>,
    /// How long a fresh connection has to make its handshake line whole
    /// (milliseconds): `AUTH_READ_TIMEOUT`, short in the tests.
    handshake_ms: AtomicU64,
    /// What the parked connections may hold at once, in bytes:
    /// [`PARKED_BYTES_PER_LANE`] per request lane, small in the tests.
    #[cfg(unix)]
    parked_bytes_max: AtomicUsize,
    /// The negative control's switch ([`Self::keep_pipelined_waits`]).
    #[cfg(all(test, unix))]
    pipelined_waits_stay: std::sync::atomic::AtomicBool,
    /// The negative control's switch ([`Self::refuse_at_the_cap`]).
    #[cfg(all(test, unix))]
    cap_refuses: std::sync::atomic::AtomicBool,
    /// See [`Self::before_wait_spawn`].
    #[cfg(test)]
    wait_spawn: Mutex<Option<WaitSpawnHook>>,
}

/// What the lanes hold right now (the Tier-1 bind's projection).
#[cfg(all(test, unix))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Census {
    /// Request-lane work, queued plus running (fresh admission reads this).
    pub(super) rpc: usize,
    /// Parked connections plus resumes no lane has taken yet.
    pub(super) parked: usize,
    /// Wait-lane work, queued (or handed to a starting lane) plus running.
    pub(super) waits: usize,
    pub(super) wait_threads: usize,
    /// Wait lanes idle, each ready for a queued wait.
    pub(super) wait_idle: usize,
    /// Request lanes serving a wait.
    pub(super) inline: usize,
    /// Whether idle connections park (a parker runs, and has not stopped).
    pub(super) parking: bool,
    /// Placed connections (not yet ended or flipped to push), excluding placing.
    pub(super) open: usize,
    /// What the parked connections held at the parker's last pass, in bytes.
    pub(super) held: usize,
    /// Parked connections that did not give way to a fresh client at the
    /// parker's last pass ([`Conn::gives_way`]).
    pub(super) spared: usize,
    /// Fresh connections admitted at the cap, handed to the parker and not
    /// placed yet.
    pub(super) placing: usize,
    /// Push streams on subscription lanes, queued plus running.
    pub(super) subscriptions: usize,
    pub(super) subscription_threads: usize,
}

/// The lanes [`Lanes::start`] started: request lanes, and subscription lanes
/// (the one reserved up front).
#[derive(Clone, Copy, Debug)]
pub(super) struct Started {
    pub(super) rpc: usize,
    pub(super) subscriptions: usize,
}

impl Lanes {
    pub(super) fn new(limits: LaneLimits) -> Arc<Self> {
        Arc::new(Self {
            limits,
            // Room for every request lane's fresh admission plus every resume a
            // parked connection can put in the queue.
            rpc: BoundedDispatch::with_resume_room(limits.rpc, limits.open),
            waits: GrowPool::new("aterm-control-wait", limits.wait),
            subscriptions: GrowPool::new("aterm-subscribe", limits.subscriptions),
            inline: Arc::new(Slots::new(limits.inline)),
            open: Arc::new(Slots::new(limits.open)),
            #[cfg(unix)]
            parker: OnceLock::new(),
            service: OnceLock::new(),
            #[cfg(all(test, unix))]
            final_replies: OnceLock::new(),
            handshake_ms: AtomicU64::new(
                u64::try_from(AUTH_READ_TIMEOUT.as_millis()).unwrap_or(u64::MAX),
            ),
            #[cfg(unix)]
            parked_bytes_max: AtomicUsize::new(limits.rpc.saturating_mul(PARKED_BYTES_PER_LANE)),
            #[cfg(all(test, unix))]
            pipelined_waits_stay: std::sync::atomic::AtomicBool::new(false),
            #[cfg(all(test, unix))]
            cap_refuses: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            wait_spawn: Mutex::new(None),
        })
    }

    /// Give fresh connections `lifetime` to make their handshake line whole
    /// (the tests' short one).
    #[cfg(all(test, unix))]
    pub(super) fn set_handshake_lifetime(&self, lifetime: Duration) {
        let ms = u64::try_from(lifetime.as_millis()).unwrap_or(u64::MAX);
        self.handshake_ms.store(ms, Ordering::Release);
    }

    /// Let the parked connections hold `bytes` at once (the tests' small
    /// budget) rather than [`PARKED_BYTES_PER_LANE`] per request lane.
    #[cfg(all(test, unix))]
    pub(super) fn set_parked_bytes_max(&self, bytes: usize) {
        self.parked_bytes_max.store(bytes, Ordering::Release);
    }

    /// Count the answers these lanes owe at exit on `replies` rather than the
    /// instance's, so a test reads its own lanes' count alone.
    #[cfg(all(test, unix))]
    pub(super) fn count_final_replies_on(&self, replies: &'static super::FinalReplies) {
        assert!(
            self.final_replies.set(replies).is_ok(),
            "set once, before any request"
        );
    }

    /// Where these lanes count the answers the exit waits for.
    fn final_replies(&self) -> &'static super::FinalReplies {
        #[cfg(all(test, unix))]
        if let Some(replies) = self.final_replies.get() {
            return replies;
        }
        &super::FINAL_REPLIES
    }

    fn handshake_lifetime(&self) -> Duration {
        Duration::from_millis(self.handshake_ms.load(Ordering::Acquire))
    }

    /// Start the request lanes, the parker and one subscription lane. Returns
    /// the lanes running; admission is published for exactly those request
    /// lanes. If the parker cannot start, idle connections stay on their lanes.
    /// One subscription lane starts here so that a lane set unable to serve a
    /// subscription is refused at startup (`control_lanes_prepared`) rather
    /// than at its first flip; the rest start on demand.
    pub(super) fn start(self: &Arc<Self>) -> Started {
        let mut started = 0;
        for index in 0..self.limits.rpc {
            let lanes = Arc::clone(self);
            match crate::qos::spawn_lane_thread(
                format!("aterm-control-{index}"),
                crate::qos::Role::Responsive,
                move || {
                    // QoS floor (qos.rs): this lane runs verb dispatch, and a
                    // read verb holds the terminal mutex while it formats. At the
                    // default band it would sit below the UI thread that takes
                    // the same mutex, leaving a descheduled holder in front of the
                    // next keystroke. Not `Interactive`: a verb storm must never
                    // outrank the UI thread.
                    lanes.rpc_lane();
                },
            ) {
                Ok(_) => started += 1,
                Err(error) => aterm_log::warn!("control worker {index} could not start: {error}"),
            }
        }
        self.rpc.set_capacity(started);
        #[cfg(unix)]
        if self.limits.open > 0 {
            self.start_parker();
        }
        let subscriptions = self
            .subscriptions
            .reserve(1, |name, first| self.grow(name, first, Self::push_lane));
        Started {
            rpc: started,
            subscriptions,
        }
    }

    /// Publish what the lanes serve with. No connection is admitted before.
    pub(super) fn publish(&self, service: Arc<dyn LaneService>) {
        let _ = self.service.set(service);
    }

    /// Admit a freshly accepted connection onto a request lane, or at the
    /// open-connection bound hand it to the parker ([`Self::admit_at_the_cap`]);
    /// or hand it back, for the listener's busy reply, when every request lane
    /// is taken by work, or at the bound when the parker cannot take it.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ControlLaneTenure",
            action = "Admit",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ControlLaneTenure",
            action = "Refuse",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    pub(super) fn admit(&self, stream: CtlStream) -> Result<(), CtlStream> {
        // With no parker an idle connection keeps its lane, so the lanes bound
        // the connections.
        let open = if self.parker_runs() {
            self.open.try_take()
        } else {
            Some(self.open.take_uncapped())
        };
        let Some(open) = open else {
            return self.admit_at_the_cap(stream);
        };
        self.rpc
            .try_submit(Job::Fresh(stream, open))
            .map_err(|job| match job {
                Job::Fresh(stream, _open) => stream,
                Job::Resume(_) => unreachable!("admission submits only fresh connections"),
            })
    }

    /// Admission at the cap: hand the fresh connection to the parker, which
    /// places it ([`Self::take_the_stalest_place`]); or hand it back for the
    /// busy reply — every request lane taken by work (as below the cap), no
    /// parker, or as many fresh ones already waiting as there are parked
    /// connections (each is owed one). As many wait at once as are parked: a
    /// placed one later goes back to the request lanes' queue as a resume, in
    /// the room it already keeps for every parked connection.
    #[cfg(unix)]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ControlLaneTenure",
            action = "Hand",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    fn admit_at_the_cap(&self, stream: CtlStream) -> Result<(), CtlStream> {
        #[cfg(test)]
        if self.cap_refuses.load(Ordering::Acquire) {
            return Err(stream);
        }
        let Some(parker) = self.parker.get() else {
            return Err(stream);
        };
        if !self.rpc.has_room() {
            return Err(stream);
        }
        let mut inbox = parker.inbox.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(waiting) = inbox.as_mut() else {
            return Err(stream);
        };
        if waiting.fresh.len() >= parker.parked.used() {
            return Err(stream);
        }
        waiting.fresh.push(stream);
        drop(inbox);
        // Non-blocking: a full bell already has a wake pending.
        let _ = (&parker.bell).write(&[1]);
        Ok(())
    }

    /// No parker off unix: the cap refuses.
    #[cfg(not(unix))]
    fn admit_at_the_cap(&self, stream: CtlStream) -> Result<(), CtlStream> {
        Err(stream)
    }

    /// A freshly admitted connection holding the open slot `open`, its
    /// handshake's lifetime counted from now.
    fn fresh_conn(&self, stream: CtlStream, open: Slot) -> Conn {
        let now = Instant::now();
        Conn {
            stream,
            scope: None,
            principal: Principal::Instance,
            caller: None,
            tenure: ConnTenure {
                id: super::NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed),
                claimed: false,
                service: Arc::clone(self.service()),
            },
            carry: Unread::default(),
            mode: SocketMode::default(),
            reply: Outbox::default(),
            header_done: false,
            expires: Some(now + self.handshake_lifetime()),
            stirred: now,
            taking: None,
            parked: None,
            _open: open,
        }
    }

    #[cfg(all(test, unix))]
    /// A lane that holds the wait lanes' lock and never lets go fails the
    /// census within the house hang detector, instead of hanging the test.
    pub(super) fn census(&self) -> Census {
        let (waits, wait_threads, wait_idle) = self.waits.census();
        let (subscriptions, subscription_threads, _) = self.subscriptions.census();
        Census {
            rpc: self.rpc.outstanding(),
            parked: self.parked(),
            waits,
            wait_threads,
            wait_idle,
            inline: self.inline.used(),
            parking: self.parker_runs(),
            open: self.open.used(),
            held: self
                .parker
                .get()
                .map_or(0, |p| p.held.load(Ordering::Acquire)),
            spared: self
                .parker
                .get()
                .map_or(0, |p| p.spared.load(Ordering::Acquire)),
            placing: self.parker.get().map_or(0, |p| {
                p.inbox
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .as_ref()
                    .map_or(0, |inbox| inbox.fresh.len())
            }),
            subscriptions,
            subscription_threads,
        }
    }

    #[cfg(all(test, unix))]
    fn parked(&self) -> usize {
        self.parker.get().map_or(0, |p| p.parked.used())
    }

    /// Raise `count` panics in the parker's loop, one per pass (the tests'
    /// injected failure; see [`Parker::faults`]).
    #[cfg(all(test, unix))]
    pub(super) fn inject_parker_panics(&self, count: usize) {
        if let Some(parker) = self.parker.get() {
            parker.faults.store(count, Ordering::Release);
        }
    }

    fn service(&self) -> &Arc<dyn LaneService> {
        self.service
            .get()
            .expect("the lane service is published before any connection is admitted")
    }

    fn rpc_lane(self: &Arc<Self>) {
        loop {
            if !self.rpc.serve_next(|job| match job {
                Job::Fresh(stream, open) => {
                    let conn = self.fresh_conn(stream, open);
                    self.drive(conn, None, Lane::Rpc);
                }
                Job::Resume(conn) => {
                    let mut conn = *conn;
                    conn.take_back();
                    self.drive(conn, None, Lane::Rpc);
                }
            }) {
                aterm_log::warn!("control worker recovered after a connection panic");
            }
        }
    }

    /// Start one thread of a grown lane set, `name`d, to live `lane`, serving
    /// `first` first when it was started for a job.
    fn grow<J: Send + 'static>(
        self: &Arc<Self>,
        name: String,
        first: Option<Handed<J>>,
        lane: fn(&Arc<Self>, Option<Handed<J>>),
    ) -> std::io::Result<()> {
        let lanes = Arc::clone(self);
        // QoS floor (qos.rs), the request lanes': a wait lane serves verbs as
        // they do, and a subscription lane's push loop reads its sessions under
        // the terminal mutex the UI thread takes.
        crate::qos::spawn_lane_thread(name, crate::qos::Role::Responsive, move || {
            lane(&lanes, first);
        })
        .map(drop)
    }

    fn wait_lane(self: &Arc<Self>, first: Option<Handed<(Conn, Request)>>) {
        self.waits.work(first, |(conn, request)| {
            self.drive(conn, Some(request), Lane::Wait)
        });
    }

    fn push_lane(self: &Arc<Self>, first: Option<Handed<Push>>) {
        self.subscriptions.work(first, |push| {
            self.service()
                .subscribe(push.request, push.scope, push.stream);
        });
    }

    /// Hand `conn` and `request` to an idle or newly spawned wait lane, or return
    /// `Err` at capacity so the caller can serve or refuse it inline.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ControlLaneTenure",
            action = "Defer",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    fn defer(self: &Arc<Self>, conn: Conn, request: Request) -> Result<(), Box<(Conn, Request)>> {
        self.waits
            .submit((conn, request), |name, first| {
                #[cfg(test)]
                self.run_wait_spawn_hook()?;
                self.grow(name, first, Self::wait_lane)
            })
            .map_err(Box::new)
    }

    /// The scheduling tests' hook ([`Self::before_wait_spawn`]), run with no
    /// lock held.
    #[cfg(test)]
    fn run_wait_spawn_hook(&self) -> std::io::Result<()> {
        let hook = self
            .wait_spawn
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        hook.map_or(Ok(()), |hook| hook())
    }

    /// Run `hook` before every wait lane's spawn from now on: blocking in it is
    /// a slow spawn, an `Err` a spawn that fails.
    #[cfg(test)]
    #[cfg_attr(not(unix), allow(dead_code))] // its tests are unix-only
    pub(super) fn before_wait_spawn(
        &self,
        hook: impl Fn() -> std::io::Result<()> + Send + Sync + 'static,
    ) {
        *self
            .wait_spawn
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(hook));
    }

    /// Serve a connection that flipped to push on a subscription lane: an idle
    /// one, or a new one while the cap allows. A flip no lane can take is
    /// answered at once and closed: a subscription may never end, so nothing
    /// waits behind one. The answer gets 100 ms to go out; nothing else is
    /// written to this connection.
    fn subscribe(self: &Arc<Self>, push: Push) {
        if let Err(push) = self
            .subscriptions
            .submit(push, |name, first| self.grow(name, first, Self::push_lane))
        {
            let mut stream = push.stream;
            let _ = stream.set_write_timeout(Some(Duration::from_millis(100)));
            let _ = stream.write_all(SUBSCRIPTION_BUSY_LINE);
        }
    }

    /// Carry one connection until it leaves this lane: closed, flipped to push,
    /// parked, or moved to a wait lane. Every fallback keeps it here.
    fn drive(self: &Arc<Self>, mut conn: Conn, mut request: Option<Request>, lane: Lane) {
        let mut may_park = true;
        let mut may_defer = true;
        loop {
            match self.serve_here(&mut conn, request.take(), lane, may_park, may_defer) {
                Here::Closed => {
                    conn.close();
                    return;
                }
                Here::Subscribe(request, scope) => {
                    let Conn { stream, tenure, .. } = conn;
                    // A push stream reads no more requests: what it bound ends
                    // here, and its open-connection slot as this returns.
                    drop(tenure);
                    self.subscribe(Push {
                        stream,
                        request,
                        scope,
                    });
                    return;
                }
                Here::Idle => match self.park(conn) {
                    Ok(()) => return,
                    Err(back) => {
                        // Its answer is delivered and its next request read
                        // here, waiting; parking is tried again once that
                        // request is answered.
                        conn = *back;
                        may_park = false;
                        may_defer = true;
                    }
                },
                Here::Wait(wait) => match self.defer(conn, wait) {
                    Ok(()) => return,
                    Err(back) => {
                        let (back, wait) = *back;
                        conn = back;
                        request = Some(wait);
                        may_defer = false;
                    }
                },
            }
        }
    }

    /// Serve `conn` on this lane until it must leave it (see [`Here`]).
    ///
    /// While a parker can take the connection (`may_park`, or once a request has been
    /// served here), the lane never waits on its peer: it delivers what the peer takes of
    /// an answer and parks the rest, and serves only a whole request, parking a connection
    /// whose next one is not. Once a park was refused (a stopped parker), the next request
    /// is read and answered here, waiting.
    fn serve_here(
        &self,
        conn: &mut Conn,
        mut pending: Option<Request>,
        lane: Lane,
        may_park: bool,
        mut may_defer: bool,
    ) -> Here {
        let service = Arc::clone(self.service());
        let Conn {
            stream,
            scope,
            principal,
            caller,
            tenure,
            carry,
            mode,
            reply,
            header_done,
            expires,
            ..
        } = conn;
        let stream: &CtlStream = stream;
        let _serving = Serving::enter(tenure.id, &mut tenure.claimed);
        // A blocking verb served here gives the lane back when this caller
        // hangs up (`super::caller_hung_up`).
        let _hangup = super::ServingFd::enter(stream);
        // A connection a lane serves was accepted on the instance's own
        // listener: its kernel peer pid may prove a self-claim
        // (`super::serving_peer_pid`).
        #[cfg(unix)]
        let _direct = super::DirectPeer::enter();
        let mut reader = LaneReader::new(stream, carry);
        let mut served = false;
        let scope = match *scope {
            Some(scope) => scope,
            None => {
                // The handshake is read once its line is whole; until then the
                // parker holds the connection, for the rest of its lifetime.
                if may_park && self.parker_runs() {
                    match gather_here(&mut reader, mode, |buffered| {
                        held(buffered, false) == Held::Whole
                    }) {
                        // Whole, or all the peer will send: read it either way.
                        Gathered::Enough | Gathered::Ended => {}
                        Gathered::Short => return Here::Idle,
                        Gathered::Broken => return Here::Closed,
                    }
                }
                // Read blocking, under the handshake's own deadline.
                if mode.set(stream, false).is_err() {
                    return Here::Closed;
                }
                let authenticated = service.authenticate(stream, &mut reader);
                mode.forget();
                let Some((authenticated, presented, inline)) = authenticated else {
                    return Here::Closed;
                };
                *scope = Some(authenticated);
                *principal = presented;
                *expires = None;
                // A verb folded into the handshake is the first request: put at
                // the head of what is buffered, it is judged whole or not and
                // served like any other. A bare `TOKEN` line is only an
                // acknowledgement.
                if let Some(verb) = inline.filter(|verb| !verb.is_empty()) {
                    reader.put_back_line(verb.as_bytes());
                }
                authenticated
            }
        };
        // The session log names this connection's Owner requests by who
        // presented its token, for as long as this lane serves it.
        let _principal = super::ServingPrincipal::enter(*principal);
        let _caller = super::ServingCaller::enter(caller);
        loop {
            let parking = (may_park || served) && self.parker_runs();
            // The last answer goes out first, and nothing more is read until the
            // peer has taken it all: what it does not take now, the parker
            // delivers as it makes room. A guarded artifact's acknowledgement
            // is the parker's to await as well, unless what the peer already
            // sent decides it. Without a parker the lane does both, waiting.
            if parking {
                if reply.owed() {
                    match deliver_here(stream, mode, reply) {
                        Ok(true) => {}
                        Ok(false) => return Here::Idle,
                        Err(_) => return Here::Closed,
                    }
                }
                if reply.awaiting_ack() {
                    match reply.ack_verdict(reader.buffer(), false) {
                        Some(acknowledged) => {
                            reply.settle(acknowledged, stream);
                        }
                        None => return Here::Idle,
                    }
                }
            } else {
                // Written and acknowledged blocking, under the handoff's own
                // deadline. What that leaves the descriptor in is not known after.
                if mode.set(stream, false).is_err() {
                    return Here::Closed;
                }
                let finished = reply.finish_waiting(stream, &mut reader);
                mode.forget();
                if finished.is_err() {
                    return Here::Closed;
                }
            }
            if reply.then_close {
                return Here::Closed;
            }
            let request = match pending.take() {
                Some(request) => request,
                None if parking => {
                    match next_request(&mut reader, mode, scope, header_done, reply, &*service) {
                        Next::Line(line) => Request::parse(line),
                        Next::Park => return Here::Idle,
                        Next::Refused => {
                            reply.then_close = true;
                            continue;
                        }
                        Next::Closed => return Here::Closed,
                    }
                }
                None => {
                    if mode.set(stream, false).is_err() {
                        return Here::Closed;
                    }
                    match read_next_line(&mut reader, reply) {
                        Next::Line(line) => Request::parse(line),
                        Next::Refused => {
                            reply.then_close = true;
                            continue;
                        }
                        Next::Park | Next::Closed => return Here::Closed,
                    }
                }
            };
            // Move wait requests (and any pipelined bytes) to a wait lane, or serve inline within `limits.inline`.
            let mut inline = None;
            if lane == Lane::Rpc && service.is_wait(&request, scope) {
                if may_defer && self.limits.wait > 0 && self.moves_with(&reader) {
                    return Here::Wait(request);
                }
                inline = self.take_inline();
                if inline.is_none() {
                    refuse_wait(reply);
                    // A refused frame's body was never read: left in the
                    // stream, the next turn would serve it as requests under
                    // this connection's scope. Closed once the busy line is
                    // out, as a frame refused on its header is.
                    reply.then_close = request.body_len() > 0;
                    may_defer = true;
                    served = true;
                    continue;
                }
            }
            may_defer = true;
            // Served blocking: a handler may wait on its socket under a timeout
            // of its own. What it leaves the descriptor in is not known after.
            if mode.set(stream, false).is_err() {
                return Here::Closed;
            }
            // Owed from BEFORE the dispatch (the action may end the process) until
            // the answer is delivered ([`Outbox::final_reply`]).
            let final_reply = self
                .final_replies()
                .enter(&request.head())
                .or_else(|| service.owed_input_reply(&request));
            let disposition = service.serve_line(request, scope, stream, &mut reader, reply);
            mode.forget();
            if reply.owed() {
                reply.final_reply = final_reply;
            }
            drop(inline);
            served = true;
            match disposition {
                None => {}
                // Closed once its answer is delivered.
                Some(ServeDisposition::Close) => reply.then_close = true,
                Some(ServeDisposition::Subscribe { request, scope }) => {
                    return Here::Subscribe(request, scope);
                }
            }
        }
    }

    /// A request lane takes one of the `limits.inline` places for a wait no
    /// wait lane can take, held while it serves the wait; `None` when every
    /// place is taken (the wait is refused: [`refuse_wait`]).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ControlLaneTenure",
            action = "InlineWait",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    fn take_inline(&self) -> Option<Slot> {
        self.inline.try_take()
    }

    /// Whether a wait request may move to a wait lane with buffered bytes behind it.
    fn moves_with(&self, reader: &LaneReader<'_>) -> bool {
        #[cfg(all(test, unix))]
        if self.pipelined_waits_stay.load(Ordering::Acquire) {
            return reader.buffer().is_empty();
        }
        let _ = reader;
        true
    }

    /// Negative-control toggle: keep waits with buffered pipelined bytes on their request lane.
    #[cfg(all(test, unix))]
    pub(super) fn keep_pipelined_waits(&self) {
        self.pipelined_waits_stay.store(true, Ordering::Release);
    }

    /// The parker's pause: while a test holds it locked, the parker takes
    /// nothing it is handed, so the test hands it several fresh connections at
    /// the cap before its next pass.
    #[cfg(all(test, unix))]
    pub(super) fn parker_pause(&self) -> &Mutex<()> {
        &self.parker.get().expect("a parker runs").pause
    }

    /// Negative-control toggle: refuse fresh clients at the connection cap instead of evicting.
    #[cfg(all(test, unix))]
    pub(super) fn refuse_at_the_cap(&self) {
        self.cap_refuses.store(true, Ordering::Release);
    }

    #[cfg(unix)]
    fn parker_runs(&self) -> bool {
        self.parker
            .get()
            .is_some_and(|parker| parker.running.load(Ordering::Acquire))
    }

    #[cfg(not(unix))]
    fn parker_runs(&self) -> bool {
        false
    }

    #[cfg(not(unix))]
    fn park(&self, conn: Conn) -> Result<(), Box<Conn>> {
        Err(Box::new(conn))
    }

    /// Hand an idle or partial connection to the parker in non-blocking mode,
    /// or return `Err` to keep it on the lane if the parker has stopped.
    #[cfg(unix)]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ControlLaneTenure",
            action = "Finish",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ControlLaneTenure",
            action = "WaitFinish",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ControlLaneTenure",
            action = "InlineFinish",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    fn park(&self, mut conn: Conn) -> Result<(), Box<Conn>> {
        let Some(parker) = self.parker.get() else {
            return Err(Box::new(conn));
        };
        // On this lane, before the inbox's lock: what it lets go of is freed
        // here, and a connection kept on its lane serves from a mapping alike.
        conn.hold_only_what_is_owed();
        let mut inbox = parker.inbox.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(waiting) = inbox.as_mut() else {
            return Err(Box::new(conn));
        };
        let Some(slot) = parker.parked.try_take() else {
            return Err(Box::new(conn));
        };
        if conn.mode.set(&conn.stream, true).is_err() {
            return Err(Box::new(conn));
        }
        conn.parked = Some(slot);
        conn.stirred = Instant::now();
        waiting.parked.push(conn);
        drop(inbox);
        // Non-blocking: a full bell already has a wake pending.
        let _ = (&parker.bell).write(&[1]);
        Ok(())
    }

    #[cfg(unix)]
    fn start_parker(self: &Arc<Self>) {
        let (bell, listen) = match std::os::unix::net::UnixStream::pair() {
            Ok(pair) => pair,
            Err(error) => {
                aterm_log::warn!("control parker has no bell: {error}");
                return;
            }
        };
        if bell.set_nonblocking(true).is_err() || listen.set_nonblocking(true).is_err() {
            aterm_log::warn!("control parker bell cannot be made non-blocking");
            return;
        }
        let parker = Arc::new(Parker {
            parked: Arc::new(Slots::new(self.limits.open)),
            inbox: Mutex::new(Some(Inbox::default())),
            held: AtomicUsize::new(0),
            #[cfg(test)]
            spared: AtomicUsize::new(0),
            #[cfg(test)]
            pause: Mutex::new(()),
            running: AtomicBool::new(true),
            bell,
            #[cfg(test)]
            faults: AtomicUsize::new(0),
        });
        let lanes = Arc::clone(self);
        let polled = Arc::clone(&parker);
        match crate::qos::spawn_lane_thread(
            "aterm-control-parker".to_string(),
            crate::qos::Role::Responsive,
            move || {
                // On the latency path of every parked driver's next request.
                lanes.parker_thread(&polled, &listen);
            },
        ) {
            // Published only once something polls it; until then (or forever,
            // if the thread could not start) idle connections stay on their
            // lanes.
            Ok(_) => {
                let _ = self.parker.set(parker);
            }
            Err(error) => aterm_log::warn!("control parker could not start: {error}"),
        }
    }

    /// The parker's thread: its loop, restarted over the SAME parked set after
    /// a panic (a connection moved out of the set when the panic came is closed
    /// by the unwind, as a lane's own connection is), until it cannot go on —
    /// then [`Self::stop_parker`], never a silent exit with connections inside.
    #[cfg(unix)]
    fn parker_thread(&self, parker: &Parker, listen: &std::os::unix::net::UnixStream) {
        let mut parked: Vec<Conn> = Vec::new();
        let mut panics = 0;
        let why = loop {
            let pass = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.parker_loop(parker, listen, &mut parked, &mut panics)
            }));
            match pass {
                Ok(why) => break why,
                Err(_) => {
                    panics += 1;
                    if panics >= PARKER_PANICS_IN_A_ROW {
                        break format!("its loop panicked {panics} times in a row");
                    }
                    aterm_log::warn!(
                        "control parker recovered after a panic; {} parked connections kept",
                        parked.len()
                    );
                }
            }
        };
        self.stop_parker(parker, parked, &why);
    }

    /// The parker cannot go on: no connection parks from now on (as without a parker), and
    /// every connection it holds, polled or not yet, is closed. Its client sees a hang-up it
    /// rides out by redialling, not a request nobody reads.
    #[cfg(unix)]
    fn stop_parker(&self, parker: &Parker, parked: Vec<Conn>, why: &str) {
        let arrivals = {
            let mut inbox = parker.inbox.lock().unwrap_or_else(PoisonError::into_inner);
            parker.running.store(false, Ordering::Release);
            inbox.take().unwrap_or_default()
        };
        parker.held.store(0, Ordering::Release);
        // Fresh connections admitted at the cap never got a place: busy.
        for stream in &arrivals.fresh {
            turn_away(stream);
        }
        let closed = parked.len() + arrivals.parked.len();
        for conn in parked.into_iter().chain(arrivals.parked) {
            // A claim's release that panics must not leave the rest open.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| conn.close()));
        }
        aterm_log::error!(
            "control parker STOPPED ({why}): {closed} parked connections closed; idle control \
             connections now keep a request lane each until aterm restarts"
        );
    }

    /// The parker's loop: one `poll(2)` over every parked connection and the
    /// bell, over the `parked` set [`Self::parker_thread`] keeps across a
    /// restart — `POLLOUT` for one whose peer owes it room for an answer,
    /// `POLLIN` for the rest, and no longer than until the first deadline (a
    /// handshake's, an artifact handoff's) runs out. Returns only when it
    /// cannot poll, saying why; every pass it completes zeroes `panics`.
    #[cfg(unix)]
    fn parker_loop(
        &self,
        parker: &Parker,
        listen: &std::os::unix::net::UnixStream,
        parked: &mut Vec<Conn>,
        panics: &mut usize,
    ) -> String {
        use std::io::Read as _;
        use std::os::fd::AsRawFd;

        // A restart may follow a drained bell whose arrivals were never taken.
        self.take_arrivals(parker, parked);
        let mut fds: Vec<libc::pollfd> = Vec::new();
        loop {
            fds.clear();
            fds.push(libc::pollfd {
                fd: listen.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
            fds.extend(parked.iter().map(|conn| libc::pollfd {
                fd: conn.stream.as_raw_fd(),
                events: if conn.reply.owed() {
                    libc::POLLOUT
                } else {
                    libc::POLLIN
                },
                revents: 0,
            }));
            let Ok(count) = libc::nfds_t::try_from(fds.len()) else {
                return format!("{} descriptors are too many to poll", fds.len());
            };
            let timeout = until_a_deadline(parked, Instant::now());
            // SAFETY: `fds` is an initialized, exclusively borrowed array of
            // `count` pollfds, and every descriptor in it is owned by `listen`
            // or by a `Conn` in `parked`, both alive across the call. -1 waits
            // until a descriptor is ready or a signal interrupts.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), count, timeout) };
            if ready < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    aterm_log::warn!("control parker poll failed: {error}");
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                continue;
            }
            #[cfg(test)]
            parker.fault();
            // Readiness of the connections this poll watched, and the
            // deadlines that ran out. Downward, so a `swap_remove` only moves
            // an entry already looked at.
            let now = Instant::now();
            for index in (0..parked.len()).rev() {
                let events = fds[index + 1].revents;
                if events == 0 && !parked[index].deadline().is_some_and(|at| at <= now) {
                    continue;
                }
                let conn = parked.swap_remove(index);
                // A panic here is this connection's alone: the unwind closes
                // it, and the parker goes on with the rest.
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.wake(conn, events, now)
                })) {
                    Ok(Some(conn)) => parked.push(conn),
                    Ok(None) => {}
                    Err(_) => aterm_log::warn!(
                        "control parker: a parked connection panicked as it woke; it is closed"
                    ),
                }
            }
            if fds[0].revents != 0 {
                let mut drain = [0u8; 64];
                while matches!((&*listen).read(&mut drain), Ok(n) if n > 0) {}
                self.take_arrivals(parker, parked);
            }
            let held = self.keep_parked_bytes_within(parked);
            parker.held.store(held, Ordering::Release);
            #[cfg(test)]
            parker.spared.store(
                parked.iter().filter(|conn| !conn.gives_way()).count(),
                Ordering::Release,
            );
            *panics = 0;
        }
    }

    /// Act on what woke a parked connection (`events`, or its deadline
    /// running out at `now`); `Some` hands it back to stay parked.
    #[cfg(unix)]
    fn wake(&self, mut conn: Conn, events: libc::c_short, now: Instant) -> Option<Conn> {
        let mut took = false;
        let step = if events & libc::POLLNVAL != 0 {
            Step::Close
        } else if events == 0 {
            Step::Stay
        } else if conn.reply.owed() {
            // Room for its answer: its peer took bytes of it.
            took = true;
            conn.deliver_parked()
        } else if conn.reply.awaiting_ack() {
            conn.ack_parked()
        } else {
            conn.gather_parked()
        };
        match step {
            Step::Serve => {
                self.resume(conn);
                None
            }
            Step::Stay if !conn.deadline().is_some_and(|at| at <= now) => {
                // Its peer sent or took bytes (or the wake was spurious, and
                // nothing came of it but a stamp).
                if events != 0 {
                    conn.stirred = now;
                }
                if took {
                    conn.taking = Some(now);
                }
                Some(conn)
            }
            // Closed: its peer hung up, its connection failed, it asked to be
            // once answered, its artifact was acknowledged or failed, or its
            // handshake or handoff did not finish in time (the handoff fails
            // closed as the connection drops it: `super::PendingHandoff`).
            Step::Close | Step::Stay => {
                close_parked(conn);
                None
            }
        }
    }

    /// A parked connection turned readable: back to the request lanes' queue,
    /// behind whatever runs, holding its parking slot until a lane takes it.
    #[cfg(unix)]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ControlLaneTenure",
            action = "Wake",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    fn resume(&self, conn: Conn) {
        if let Err(job) = self.rpc.submit_resumed(Job::Resume(Box::new(conn))) {
            // Unreachable while the open bound sizes the queue; close rather
            // than silently strand a live connection if that ever breaks.
            aterm_log::warn!("control parker: resume refused, closing");
            if let Job::Resume(conn) = job {
                (*conn).close();
            }
        }
    }
}

/// What a lane found for its connection's next request.
enum Next {
    /// A whole request, its line read (the body it announces is buffered).
    Line(String),
    /// Not whole yet, and nothing more there now: the parker gathers the rest.
    Park,
    /// A frame's header, or a line too long for any request, was refused, its
    /// answer in the outbox: close.
    Refused,
    /// Nothing more will come, or the connection failed.
    Closed,
}

/// The connection's next request, without waiting for its peer: served from
/// what `reader` holds once it is whole, with what the peer has already sent
/// gathered in first. A frame's header is acted on as soon as it is whole
/// ([`LaneService::frame_header`]), before its body is waited for.
fn next_request(
    reader: &mut LaneReader<'_>,
    mode: &mut SocketMode,
    scope: Scope,
    header_done: &mut bool,
    reply: &mut Outbox,
    service: &dyn LaneService,
) -> Next {
    loop {
        match held(reader.buffer(), true) {
            Held::Whole => {
                *header_done = false;
                return read_next_line(reader, reply);
            }
            Held::Header(end) if !*header_done => {
                let header =
                    Request::parse(request_line_text(&reader.buffer()[..end]).into_owned());
                if !service.frame_header(&header, scope, reply) {
                    return Next::Refused;
                }
                *header_done = true;
            }
            Held::Header(_) | Held::Partial => {}
        }
        let done = *header_done;
        match gather_here(reader, mode, |buffered| servable(buffered, true, done)) {
            Gathered::Enough => {}
            Gathered::Short => return Next::Park,
            // All the peer will send is here: what it left is served as it is (a
            // last line with no newline; a frame cut short closes), and an
            // empty end is the end.
            Gathered::Ended if reader.buffer().is_empty() => return Next::Closed,
            Gathered::Ended => {
                *header_done = false;
                return read_next_line(reader, reply);
            }
            Gathered::Broken => return Next::Closed,
        }
    }
}

/// The request line `reader` holds next, read on a lane: one too long for any
/// request is answered in the outbox ([`Next::Refused`]), never a bare close.
fn read_next_line(reader: &mut LaneReader<'_>, reply: &mut Outbox) -> Next {
    let mut refusal = Vec::new();
    match read_authenticated_request_line(reader, &mut refusal) {
        Some(line) => Next::Line(line),
        None if refusal.is_empty() => Next::Closed,
        None => {
            reply.push_owned(refusal);
            Next::Refused
        }
    }
}

/// [`Unread::gather`] on a lane: its socket stops blocking first, and stays so
/// for the delivery and the park that follow.
#[cfg(unix)]
fn gather_here(
    reader: &mut LaneReader<'_>,
    mode: &mut SocketMode,
    enough: impl Fn(&[u8]) -> bool,
) -> Gathered {
    if mode.set(reader.stream, true).is_err() {
        return Gathered::Broken;
    }
    reader.unread.gather(reader.stream, enough)
}

/// No parker runs off unix, so no lane gathers without waiting.
#[cfg(not(unix))]
fn gather_here(
    _reader: &mut LaneReader<'_>,
    _mode: &mut SocketMode,
    _enough: impl Fn(&[u8]) -> bool,
) -> Gathered {
    unreachable!("a lane gathers only while a parker runs")
}

/// Deliver the outbox from a lane while the parker can have the rest: what
/// the peer takes now, the socket non-blocking.
fn deliver_here(
    stream: &CtlStream,
    mode: &mut SocketMode,
    reply: &mut Outbox,
) -> std::io::Result<bool> {
    mode.set(stream, true)?;
    reply.deliver(stream, true)
}

/// What the parker does with a connection it looked at.
#[cfg(unix)]
enum Step {
    /// Its next request (or a frame's header) is whole: back to the lanes.
    Serve,
    /// Keep it parked.
    Stay,
    Close,
}

#[cfg(unix)]
impl Conn {
    /// The bytes it holds of its peer's and for its peer: the answer not yet
    /// taken and what has come of its next request, as allocated or mapped.
    fn held_bytes(&self) -> usize {
        self.carry.held() + self.reply.held()
    }

    /// Let go of every buffer it holds no byte in — an idle connection holds
    /// none (its next answer is made on a lane, in a buffer of its own) — and
    /// move a large unsent answer off the heap ([`Outbox::spill`]), and a
    /// large request a lane began to read before it parked
    /// ([`Unread::room_for_the_request`], as the parker's own reads would).
    fn hold_only_what_is_owed(&mut self) {
        if self.reply.owed() {
            self.reply.spill();
        }
        self.carry.hold_only_what_is_owed(self.scope.is_some());
    }

    /// Whether a fresh client at the connection cap may evict this parked connection
    /// (authenticated and holding no live supervisor/drive claim).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ControlLaneTenure",
            action = "Spare",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ControlLaneTenure",
            action = "Unspare",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    fn gives_way(&self) -> bool {
        self.scope.is_some() && !self.has_live_claim()
    }

    /// Whether a claim still ends with it. The tenure's cleanup flag stays set
    /// until the connection drops, even after its last claim was released, so
    /// this asks the service's current authority, not that history bit.
    fn has_live_claim(&self) -> bool {
        self.tenure.claimed && self.tenure.service.has_live_claim(self.tenure.id)
    }

    /// Close it to make room for another: its peer sees the hang-up any close
    /// is, and its open and parked slots are handed over to the connection
    /// taking its place (`None`, both given back, if it held no parked one).
    fn give_up_place(mut self) -> Option<(Slot, Slot)> {
        aterm_uds::hangup::hang_up(&self.stream);
        let place = self.parked.take();
        let Self { _open: open, .. } = self;
        Some((open, place?))
    }

    /// Deliver what the peer has room for of the answer it is owed.
    fn deliver_parked(&mut self) -> Step {
        let delivered = self.reply.deliver(&self.stream, true);
        if matches!(delivered, Ok(true)) {
            self.hold_only_what_is_owed();
        }
        match delivered {
            Ok(false) => Step::Stay,
            // A guarded artifact is out: what the peer sent already may decide
            // its acknowledgement; otherwise the parker waits for the line.
            Ok(true) if self.reply.awaiting_ack() => {
                match self.reply.ack_verdict(self.carry.as_slice(), false) {
                    Some(acknowledged) => {
                        self.reply.settle(acknowledged, &self.stream);
                        Step::Close
                    }
                    None => Step::Stay,
                }
            }
            Ok(true) if self.reply.then_close => Step::Close,
            // Delivered: the next request may be whole already (read behind
            // the answer); otherwise the parker waits for it.
            Ok(true)
                if servable(
                    self.carry.as_slice(),
                    self.scope.is_some(),
                    self.header_done,
                ) =>
            {
                Step::Serve
            }
            Ok(true) => Step::Stay,
            Err(_) => Step::Close,
        }
    }

    /// Read what has come of the line that acknowledges a guarded artifact,
    /// and settle it once that says anything: the guard released, or
    /// quarantined. The connection closes either way.
    fn ack_parked(&mut self) -> Step {
        let reply = &self.reply;
        let gathered = self.carry.gather(&self.stream, |sent| {
            reply.ack_verdict(sent, false).is_some()
        });
        let acknowledged = match gathered {
            Gathered::Short => return Step::Stay,
            Gathered::Enough => self.reply.ack_verdict(self.carry.as_slice(), false),
            Gathered::Ended => self.reply.ack_verdict(self.carry.as_slice(), true),
            Gathered::Broken => None,
        };
        self.reply.settle(acknowledged == Some(true), &self.stream);
        Step::Close
    }

    /// Read what its peer has sent of its next request, until it is whole or
    /// nothing more is there. A request that needs [`SPILL_FROM`] or more is
    /// gathered off the heap, into a mapping sized for the whole of it
    /// ([`Unread::room_for_the_request`]).
    fn gather_parked(&mut self) -> Step {
        let (authenticated, header_done) = (self.scope.is_some(), self.header_done);
        match self.carry.gather_with(
            &self.stream,
            |buffered| servable(buffered, authenticated, header_done),
            |unread| unread.room_for_the_request(authenticated),
        ) {
            Gathered::Enough => Step::Serve,
            Gathered::Short => Step::Stay,
            // The peer sends no more: a lane serves what it left (and then
            // reads the end), or there is nothing left to serve.
            Gathered::Ended if !self.carry.is_empty() => Step::Serve,
            Gathered::Ended | Gathered::Broken => Step::Close,
        }
    }
}

/// How long the parker may wait before the first deadline it holds runs out
/// (a handshake's, an artifact handoff's): a `poll(2)` timeout in
/// milliseconds, rounded up so it never wakes early; -1 (no limit) when no
/// connection it holds has one.
#[cfg(unix)]
fn until_a_deadline(parked: &[Conn], now: Instant) -> libc::c_int {
    parked
        .iter()
        .filter_map(Conn::deadline)
        .min()
        .map_or(-1, |at| {
            let left = at.saturating_duration_since(now).as_millis();
            libc::c_int::try_from(left.saturating_add(1)).unwrap_or(libc::c_int::MAX)
        })
}

#[cfg(unix)]
impl Lanes {
    /// Move what lanes handed the parker into its polled set, then place each
    /// fresh connection admitted at the cap.
    fn take_arrivals(&self, parker: &Parker, parked: &mut Vec<Conn>) {
        let fresh = {
            // Held across the take, so a pass already under way when a test takes the pause
            // finishes taking before the test hands anything over, and none takes after.
            #[cfg(test)]
            let _pause = parker.pause.lock().unwrap_or_else(PoisonError::into_inner);
            let mut inbox = parker.inbox.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(inbox) = inbox.as_mut() else {
                return;
            };
            // `append` leaves the inbox its capacity for the next arrivals.
            parked.append(&mut inbox.parked);
            std::mem::take(&mut inbox.fresh)
        };
        for stream in fresh {
            self.take_the_stalest_place(parker, parked, stream);
        }
    }

    /// Place a connection admitted at the cap into a freed slot or by evicting the
    /// stalest parked connection that [`Conn::gives_way`], or reply busy.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ControlLaneTenure",
            action = "Place",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ControlLaneTenure",
            action = "TurnAway",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    fn take_the_stalest_place(&self, parker: &Parker, parked: &mut Vec<Conn>, stream: CtlStream) {
        let place = self.free_place(parker).or_else(|| {
            let index = stalest(parked, |conn| conn.gives_way().then_some(false))?;
            let victim = parked.swap_remove(index);
            note_eviction("a fresh client at the connection cap");
            // A close that panics costs that connection alone, as a wake's
            // does: its slots went back as it unwound, and are taken again.
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| victim.give_up_place()))
                .unwrap_or_else(|_| {
                    aterm_log::warn!(
                        "control parker: a connection closed for a fresh client panicked as it \
                         closed; it is closed"
                    );
                    None
                })
                .or_else(|| self.free_place(parker))
        });
        let Some((open, place)) = place else {
            turn_away(&stream);
            return;
        };
        let mut conn = self.fresh_conn(stream, open);
        conn.parked = Some(place);
        if conn.mode.set(&conn.stream, true).is_err() {
            close_parked(conn);
            return;
        }
        parked.push(conn);
    }

    /// An open slot and a parked one, both free: the cap has room again.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ControlLaneTenure",
            action = "PlaceFreed",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    fn free_place(&self, parker: &Parker) -> Option<(Slot, Slot)> {
        let open = self.open.try_take()?;
        let place = parker.parked.try_take()?;
        Some((open, place))
    }

    /// Enforce the parked byte budget by closing the stalest byte-holding connections
    /// (sparing active readers and claim holders until last, and never evicting the
    /// sole byte holder).
    fn keep_parked_bytes_within(&self, parked: &mut Vec<Conn>) -> usize {
        let budget = self.parked_bytes_max.load(Ordering::Acquire);
        let holds = |conn: &Conn| conn.held_bytes() > 0;
        let mut held: usize = parked.iter().map(Conn::held_bytes).sum();
        let now = Instant::now();
        let taking = |conn: &Conn| {
            conn.taking
                .is_some_and(|at| now.saturating_duration_since(at) < TAKING_SPAN)
        };
        while held > budget && parked.iter().filter(|conn| holds(conn)).nth(1).is_some() {
            let Some(index) = stalest(parked, |conn| {
                holds(conn).then(|| (conn.has_live_claim(), taking(conn)))
            }) else {
                break;
            };
            let victim = parked.swap_remove(index);
            held -= victim.held_bytes();
            note_eviction("the parked connections' byte budget");
            // A close that panics (a claim's release) costs that connection
            // alone, as a wake's does.
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| close_parked(victim)))
                .is_err()
            {
                aterm_log::warn!(
                    "control parker: a connection closed for the byte budget panicked as it \
                     closed; it is closed"
                );
            }
        }
        held
    }
}

#[cfg(unix)]
impl<T> BoundedDispatch<T> {
    /// Whether a fresh admission would find a request lane now: the cap
    /// admits one only then ([`Lanes::admit_at_the_cap`]).
    fn has_room(&self) -> bool {
        self.outstanding.load(Ordering::Acquire) < self.capacity.load(Ordering::Acquire)
    }
}

/// Index of the parked connection with the lowest `rank` and oldest [`Conn::stirred`] timestamp.
#[cfg(unix)]
fn stalest<R: Ord + Copy>(parked: &[Conn], rank: impl Fn(&Conn) -> Option<R>) -> Option<usize> {
    parked
        .iter()
        .enumerate()
        .filter_map(|(index, conn)| rank(conn).map(|rank| (index, rank, conn.stirred)))
        .min_by_key(|&(_, rank, stirred)| (rank, stirred))
        .map(|(index, _, _)| index)
}

/// Parked connections closed to make room, since start.
#[cfg(unix)]
static EVICTIONS: AtomicU64 = AtomicU64::new(0);

/// Count one parked connection closed to make room for `why`, and say so in
/// the log at the first, second, fourth, eighth… so a process squatting the
/// cap shows there without a line per client it displaced.
#[cfg(unix)]
fn note_eviction(why: &str) {
    let count = EVICTIONS.fetch_add(1, Ordering::Relaxed) + 1;
    if count.is_power_of_two() {
        aterm_log::warn!(
            "control parker closed the stalest parked connection for {why} ({count} so far)"
        );
    }
}

/// Answer a fresh connection the parker cannot place with the busy reply,
/// without waiting: it never got as far as a request, and its empty send
/// buffer takes the line.
#[cfg(unix)]
fn turn_away(stream: &CtlStream) {
    crate::control_listener::note_busy_reply();
    if stream.set_nonblocking(true).is_ok() {
        let mut writer = stream;
        let _ = writer.write_all(aterm_types::control_verbs::CONTROL_BUSY_LINE.as_bytes());
    }
}

#[cfg(all(test, unix))]
impl Parker {
    /// The injected failure: one panic per pass while any are owed.
    fn fault(&self) {
        let owed = self
            .faults
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1));
        if owed.is_ok() {
            panic!("injected control parker panic");
        }
    }
}

/// A wait no lane may take (every wait lane taken and the request lanes' wait budget
/// spent) is answered busy at once: nothing was served, and the client may send it again.
/// The connection stays; only a failed delivery ends it.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ControlLaneTenure",
        action = "BusyWait",
        project = "aterm_gui::control::control_lanes::conformance::project"
    )
)]
fn refuse_wait(out: &mut Outbox) {
    out.extend_from_slice(aterm_types::control_verbs::CONTROL_BUSY_LINE.as_bytes());
}

/// A parked connection's peer hung up with nothing left to read: closed where
/// it is, its tenure (and any claim bound to it) ended with it.
#[cfg(unix)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ControlLaneTenure",
        action = "ParkedClose",
        project = "aterm_gui::control::control_lanes::conformance::project"
    )
)]
fn close_parked(conn: Conn) {
    conn.close();
}

/// The serving thread's view of the connection it carries, which a
/// connection-scoped claim binds to ([`super::serving_connection`]). Set while
/// a lane serves; the claim flag is carried back into the connection when it
/// leaves the lane (normally or by unwinding).
struct Serving<'a> {
    claimed: &'a mut bool,
}

impl<'a> Serving<'a> {
    fn enter(id: u64, claimed: &'a mut bool) -> Self {
        super::SERVING_CONNECTION.with(|serving| serving.set(Some(id)));
        super::CONNECTION_CLAIMED.with(|flag| flag.set(*claimed));
        Self { claimed }
    }
}

impl Drop for Serving<'_> {
    fn drop(&mut self) {
        *self.claimed = super::CONNECTION_CLAIMED.with(|flag| flag.replace(false));
        super::SERVING_CONNECTION.with(|serving| serving.set(None));
    }
}

#[cfg(all(test, unix))]
#[path = "control_lanes_conformance.rs"]
mod conformance;

#[cfg(all(test, unix))]
mod outbox_spill_tests {
    use super::{CtlStream, Outbox, SPILL_FROM, Segment};
    use std::io::Read as _;

    /// An answer the socket does not take at once leaves the heap as its
    /// connection parks: its heap segments are let go, one mapping holds
    /// exactly the unsent rest, every byte arrives in order, and the mapping
    /// goes once it is delivered. One shorter than [`SPILL_FROM`] stays where
    /// it is, and so does an answer holding a secret.
    #[test]
    fn a_large_unsent_answer_leaves_the_heap_and_arrives_whole() {
        let (server, mut peer) = CtlStream::pair().expect("private control pair");
        // A hang detector, never a latency budget.
        let hang_detector = Some(aterm_spec::wait::MUST_HAPPEN);
        server.set_write_timeout(hang_detector).unwrap();
        peer.set_read_timeout(hang_detector).unwrap();
        let answer: Vec<u8> = (0..256 * 1024_u32)
            .map(|i| u8::try_from(i % 251).expect("below 251"))
            .collect();
        let mut out = Outbox::default();
        out.push_owned(answer[..1024].to_vec());
        out.push_owned(answer[1024..].to_vec());
        server.set_nonblocking(true).unwrap();
        assert!(
            !out.deliver(&server, true).unwrap(),
            "the socket takes part of it"
        );
        let unsent = out
            .segments
            .iter()
            .map(|segment| segment.as_bytes().len())
            .sum::<usize>()
            - out.sent;
        out.spill();
        assert!(
            out.segments.len() == 1 && matches!(out.segments.front(), Some(Segment::Spilled(_))),
            "one mapping, no heap segment left"
        );
        assert_eq!(out.held(), unsent, "the mapping holds the unsent rest");
        assert!(out.owed());
        let reader = std::thread::spawn(move || {
            let mut got = vec![0u8; answer.len()];
            peer.read_exact(&mut got).expect("the whole answer");
            (answer, got)
        });
        server.set_nonblocking(false).unwrap();
        assert!(out.deliver(&server, false).unwrap());
        assert!(out.segments.is_empty(), "the mapping goes once delivered");
        let (answer, got) = reader.join().expect("the reader");
        assert!(got == answer, "every byte, in order");

        let mut small = Outbox::default();
        small.push_owned(vec![b'x'; SPILL_FROM - 1]);
        small.spill();
        assert!(
            small
                .segments
                .iter()
                .all(|segment| matches!(segment, Segment::Bytes(_))),
            "a short answer stays on the heap"
        );
        let mut secret = Outbox::default();
        secret.push_secret(
            aterm_uds::sudo::WireBuffer::from_bytes(b"OK askpass secret=74657374\n")
                .expect("a short line"),
        );
        secret.push_owned(vec![b'x'; SPILL_FROM]);
        secret.spill();
        assert!(
            secret
                .segments
                .iter()
                .all(|segment| !matches!(segment, Segment::Spilled(_)))
                && matches!(secret.segments.front(), Some(Segment::Secret(_))),
            "an answer holding a secret keeps it in its guarded storage"
        );
    }
}

#[cfg(test)]
mod outbox_delivery_tests {
    use super::{CtlStream, Outbox, Segment};
    use std::io::Read as _;

    /// A secret-bearing answer (`askpass`) reaches its peer byte for byte
    /// through a real socket, and once it is out the outbox holds none of it:
    /// it was moved into its own guarded segment (`WireBuffer`, scrubbed as it
    /// drops), never copied into an ordinary byte segment, and that segment
    /// went as soon as its last byte was taken.
    #[test]
    fn a_secret_answer_is_delivered_whole_and_kept_nowhere() {
        let (server, mut peer) = CtlStream::pair().expect("private control pair");
        // A hang detector, never a latency budget.
        let hang_detector = Some(aterm_spec::wait::MUST_HAPPEN);
        server.set_write_timeout(hang_detector).unwrap();
        peer.set_read_timeout(hang_detector).unwrap();
        let reply = b"OK askpass secret=74657374\n";
        let mut out = Outbox::default();
        out.push_secret(aterm_uds::sudo::WireBuffer::from_bytes(reply).expect("a short line"));
        assert!(
            out.segments
                .iter()
                .all(|segment| matches!(segment, Segment::Secret(_))),
            "the secret is moved into its own segment, never copied into a byte one"
        );

        assert!(out.deliver(&server, false).unwrap());
        let mut received = vec![0; reply.len()];
        peer.read_exact(&mut received).unwrap();
        assert_eq!(received, reply, "delivery must preserve every wire byte");
        assert!(out.segments.is_empty(), "a delivered segment is dropped");
        assert_eq!(out.sent, 0);
    }
}
