// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The control socket's lanes: a request holds a thread, a connection does
//! not. Supervised agent tabs each hold a persistent `RelayCtl` that is idle
//! most of the time, so a connection must not pin a thread while idle.
//!
//! **A lane never waits on its peer.** It is given a connection only with a
//! WHOLE request in hand — the line, and the body a frame announces (the
//! handshake: its line) — and it writes the answer without waiting for the peer
//! to read it. Everything else a peer can make a lane wait for is the PARKER's,
//! multiplexed in its one `poll(2)`: the rest of a handshake, line or frame it
//! began, and the part of an answer it has not read yet. Before (measured on a
//! headless instance), eight peers that never finished their `AUTH` line (no
//! token needed), stopped mid-line or mid-frame, or did not read a large answer
//! (`help --full`, 190 KB) held all eight request lanes, and every other client
//! was answered `ERR control server busy; retry` for as long as they liked; a
//! deadline on each wait only made them reconnect when it ran out (eight such
//! peers still left 0 of 50 fresh requests answered over 100 s), and a write
//! bound cut off a pager that paused and was outlived by a reader taking a byte
//! at a time. The exceptions are the waits that ARE the request: a relay, which
//! carries its peer's bytes both ways on a wait lane, and a guarded artifact's
//! handoff, written and acknowledged under `ARTIFACT_HANDOFF_TIMEOUT` — at most
//! `ARTIFACT_HANDOFF_LIMIT` (four) at once, so never every request lane.
//!
//! **Where a connection is, at any moment — exactly one of:**
//!
//! * on a request lane (`CONTROL_WORKERS` fixed threads), authenticating or
//!   serving a request. Admission counts these, queued plus running.
//! * on a WAIT lane ([`WaitLanes`], at most `CONTROL_WAIT_WORKERS` threads, grown
//!   on demand and kept): serving a request whose time is spent WAITING on an
//!   event — `await`, `ready`, `wait`, `turn`, a `post` or `inbox get @…` that
//!   parks on the bridge, a paste into another session waiting (bounded) for its
//!   program to read it — or relaying the connection to a session this
//!   instance does not host. A request lane that reads
//!   such a line hands the connection over and is free again at once. No lane
//!   waits on a program to WRITE to it: a verb's input never parks its lane
//!   (`control::background_input`).
//! * on a request lane SERVING A WAIT no wait lane can take — every one is
//!   taken. At most `LaneLimits::inline` request lanes do so at once
//!   (`CONTROL_INLINE_WAITS`, half of them); a wait past that gets the busy reply
//!   on its connection, promptly, and the connection stays open. Before that
//!   bound, the waits past the wait lanes (a 65th at once) took request lanes
//!   without limit: eight of them held every lane for up to the waits' 600 s
//!   clamp, and the next request of every parked driver — never refused, only
//!   queued — waited behind them. Now the other half of the lanes is always free
//!   to serve requests. A wait with requests PIPELINED behind it moves like any
//!   other, taking with it the bytes its request lane had already read past it
//!   ([`Conn::carry`]): while such waits stayed on their request lane the
//!   budget's four places turned the fifth pipelining client away with most of
//!   the wait lanes free.
//! * PARKED ([`Parker`], one thread): between requests — idle, or waiting on
//!   its peer: for the rest of its next request (a handshake not yet whole
//!   included), or to take the rest of its last answer ([`Outbox`]). One
//!   `poll(2)` over every parked descriptor: `POLLOUT` while an answer is
//!   owed (nothing more is read until it is delivered, so a connection holds at
//!   most one answer, and of its requests no more than the next one needs and
//!   one read past it), `POLLIN` otherwise, reading what arrives into
//!   [`Conn::carry`]. A connection whose next request is whole goes
//!   back to the request lanes' queue (a frame's header goes back before its
//!   body: see [`LaneService::frame_header`]); one whose peer hung up with
//!   nothing left to serve, or whose handshake is not whole when its
//!   `AUTH_READ_TIMEOUT` (from admission) runs out, is closed right there.
//! * on the subscription pool (unchanged: a `subscribe` flips it to push).
//!
//! **Admission.** A fresh connection gets `ERR control server busy; retry`
//! when every request lane is taken by work, or when the open-connection cap
//! is reached (`control_connections_cap`: a quarter of the soft descriptor
//! limit, at most `CONTROL_CONNECTIONS_MAX`). That cap is the parker's
//! capacity, so an idle connection always has a place to park and never keeps
//! a lane (which would also starve the resumes queued behind it). The cap
//! bounds open connections, not requests, and leaves most descriptors to tabs
//! and files; it is also what a peer that keeps connections open costs —
//! idle, half-sent or unread alike — never a lane.
//!
//! **If the parker fails.** Every parked connection lives in the parker, so a
//! parker that died would strand them all — every supervisor loop and every
//! long-lived client hung on a request nobody reads — and every connection
//! parked after it. So its failures are bounded: a panic while one connection
//! wakes costs that connection (closed, its claims released — what a lane's
//! panic costs its own), as on the lanes; any other panic restarts its loop over
//! the same parked set; and a parker that cannot go on (`PARKER_PANICS_IN_A_ROW`
//! panics with no pass completed between them, or descriptors it cannot poll)
//! STOPS, loudly (an `error` line) and safely: it closes every connection it
//! holds (a hang-up, which every client already rides out by dialling again)
//! and the lanes run as without a parker from then on — never a connection
//! handed to a thread that is gone.
//!
//! **What is kept.** Per-connection request ORDER: one owner at a time; a
//! connection moves only between requests; it parks only without a whole
//! request in hand (a pipelined next request already whole is served where it
//! is), taking what it holds of the next one along in [`Conn::carry`]; a wait
//! that moves takes what its lane had read past it along, served first where it
//! lands, before anything more is read from the socket; and an answer is
//! delivered whole, in order, before the next request is read. The auth
//! and uid checks (the listener's, and the handshake on the first lane). Claims
//! bound to a connection (`meta set supervisor` without `ttl=`) stay bound across
//! every move and are released when the connection ends, however it ends — the
//! release is a drop guard ([`ConnTenure`]), so a lane's panic or the parker's
//! close releases them too. The cut `RelayCtl`'s interrupter makes (a client-side
//! `shutdown`) reads, wherever the connection is, as the hang-up it always was.
//! Every queue is bounded and every thread count is capped. Without a parker
//! (off unix, one that could not start, or one that stopped) a lane reads and
//! writes its connection waiting, an idle connection stays on its lane, and the
//! connection bound is the lanes', exactly as before (a handshake still has
//! `AUTH_READ_TIMEOUT` from the lane's pick-up to be whole).
//!
//! Derived model: `aterm_models_native::control_lane_tenure_model` (a running
//! parker's lanes; a stopped parker's fallback is the design it rejects, on
//! purpose); its Tier-1 bind (`control_lanes_conformance.rs`) drives these lanes
//! with real sockets.

use std::collections::VecDeque;
use std::io::{BufRead as _, BufReader, Read, Write as _};
#[cfg(unix)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use aterm_uds::CtlStream;

use super::{
    AUTH_READ_TIMEOUT, BoundedDispatch, MAX_REQUEST_LINE, Principal, Scope, ServeDisposition,
    announced_body, decode_request_line, read_authenticated_request_line,
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
    /// Serve one authenticated request, writing its answer into `out` (the
    /// lane delivers it, without waiting for the peer to read it). `reader`
    /// holds the whole request: its line was read from it, and the body a
    /// frame announces is in it. `None` keeps the connection.
    fn serve_line(
        &self,
        line: String,
        scope: Scope,
        stream: &CtlStream,
        reader: &mut LaneReader<'_>,
        out: &mut Vec<u8>,
    ) -> Option<ServeDisposition>;
    /// A frame's header `line` has arrived, and not all of the body it
    /// announces: act on the header now, as the input boundary it is, while the
    /// parker waits for the body (the request is served whole once it has come,
    /// header and all). `false` refuses the frame, its answer in `out`: the
    /// connection closes once that is delivered. Nothing to act on by default.
    fn frame_header(&self, line: &str, scope: Scope, out: &mut Vec<u8>) -> bool {
        let _ = (line, scope, out);
        true
    }
    /// Whether `line` belongs on a wait lane (see the module doc).
    fn is_wait(&self, line: &str, scope: Scope) -> bool;
    /// Take over a connection that flipped to push.
    fn subscribe(&self, line: String, scope: Scope, stream: CtlStream);
    /// Release every claim bound to connection `id`: it ended.
    fn release(&self, id: u64);
}

/// The caps. Production: `CONTROL_WORKERS`, `CONTROL_WAIT_WORKERS`,
/// `CONTROL_INLINE_WAITS`, `control_connections_cap()`; the scheduling tests use
/// small ones.
#[derive(Clone, Copy, Debug)]
pub(super) struct LaneLimits {
    /// Request lanes.
    pub(super) rpc: usize,
    /// Wait lanes, at most.
    pub(super) wait: usize,
    /// Request lanes serving a wait at once, at most (a wait no wait lane can
    /// take); a wait past it gets the busy reply. Production keeps it below
    /// `rpc`, so a request lane is always left for requests; the scheduling
    /// tests' old-design rigs set it to `rpc`, the fallback as first shipped.
    pub(super) inline: usize,
    /// Connections open at once while the parker runs — its capacity.
    pub(super) open: usize,
}

/// How many times in a row the parker's loop may panic, with no pass completed
/// between them, before it stops (see the module doc). A panic that one
/// connection's wake raised never counts: that connection pays for it alone.
#[cfg(unix)]
const PARKER_PANICS_IN_A_ROW: usize = 3;

/// The read buffer a lane serves a connection through (`BufReader::new`'s),
/// and the most read from a peer at once while a request is gathered.
const READ_BUFFER: usize = 8 * 1024;

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

/// What `buffered` holds of the request at its head: the wire's grammar, so
/// a lane and the parker agree on when a request is whole. Before the
/// handshake only its line counts: a verb folded into it is the connection's
/// first request, judged when the lane unfolds it.
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
    let body = announced_body(&decode_request_line(buffered[..end].to_vec()));
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

/// A connection's answers on their way to its peer, from the first byte the
/// peer has not taken yet. A lane writes a request's answer here and delivers
/// what the peer takes at once; the parker delivers the rest as the peer makes
/// room. Nothing more is read from the connection until it is all delivered.
#[derive(Default)]
pub(super) struct Outbox {
    bytes: Vec<u8>,
    sent: usize,
    /// Close the connection once the answer is delivered: its request ended it.
    then_close: bool,
    /// The answer is one the exit waits for ([`super::FinalReplies`]): its request
    /// can end the process. Held until the answer is delivered, by the lane or by
    /// the parker, or the connection is gone.
    final_reply: Option<super::FinalReply>,
}

impl Outbox {
    /// Whether some of the answer is still the peer's to take.
    fn owed(&self) -> bool {
        self.sent < self.bytes.len()
    }

    /// Zero initialized reply bytes before clearing, reusing or freeing the
    /// buffer. Replies can contain askpass secrets; `clear()` alone leaves
    /// them in the allocation. Volatile stores prevent elision of this pass.
    fn scrub(&mut self) {
        for byte in self.bytes.iter_mut() {
            // SAFETY: `byte` is a valid, aligned, initialized place for a `u8`
            // for the whole call; the volatile store the optimizer may not drop.
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
    }

    /// Deliver what the peer takes: `Ok(true)` once all of it is out,
    /// `Ok(false)` when a non-blocking socket has no room left. A blocking
    /// socket that reports no room (a write timeout) fails.
    fn deliver(&mut self, stream: &CtlStream, nonblocking: bool) -> std::io::Result<bool> {
        let mut writer = stream;
        while self.owed() {
            match writer.write(&self.bytes[self.sent..]) {
                Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
                Ok(written) => self.sent += written,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) if nonblocking && error.kind() == std::io::ErrorKind::WouldBlock => {
                    return Ok(false);
                }
                Err(error) => return Err(error),
            }
        }
        // The delivered bytes may include a relayed secret; zero them before
        // the buffer is cleared or freed (neither `clear()` nor `Vec::new()`
        // does). Done once the whole answer is out, so a `WouldBlock` return
        // above leaves the not-yet-sent bytes in place.
        self.scrub();
        // A large answer's buffer is not kept for the next one.
        if self.bytes.capacity() > 8 * READ_BUFFER {
            self.bytes = Vec::new();
        } else {
            self.bytes.clear();
        }
        self.sent = 0;
        // Delivered: the exit no longer waits for it.
        self.final_reply = None;
        Ok(true)
    }
}

impl Drop for Outbox {
    fn drop(&mut self) {
        // A connection dropped before its answer was fully delivered (an error
        // or hangup mid-reply) must not leave a relayed secret in freed heap.
        self.scrub();
    }
}

#[cfg(test)]
mod outbox_scrub_tests {
    use super::Outbox;

    /// After an answer is delivered, `sent == bytes.len()`; `scrub` must still
    /// zero the WHOLE buffer, not the (now empty) unsent tail — a relayed
    /// secret lives in the delivered span. Guards against scrubbing
    /// `bytes[sent..]`, which with `sent == len` would zero nothing.
    #[test]
    fn scrub_zeroes_the_whole_delivered_buffer() {
        let mut out = Outbox::default();
        out.bytes
            .extend_from_slice(b"OK askpass secret=73336372337420\n");
        out.sent = out.bytes.len();
        out.scrub();
        assert!(out.bytes.iter().all(|&b| b == 0), "{:?}", out.bytes);
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

/// Read what the peer has sent into `carry`, until `enough(carry)` or nothing
/// more is there. The socket must not block (a parked one does not; a lane's
/// is switched for the call: [`without_waiting`]).
#[cfg(unix)]
fn gather(stream: &CtlStream, carry: &mut Vec<u8>, enough: impl Fn(&[u8]) -> bool) -> Gathered {
    let mut reader = stream;
    let mut chunk = [0u8; READ_BUFFER];
    loop {
        if enough(carry) {
            return Gathered::Enough;
        }
        match reader.read(&mut chunk) {
            Ok(0) => return Gathered::Ended,
            Ok(read) => carry.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                return Gathered::Short;
            }
            Err(_) => return Gathered::Broken,
        }
    }
}

/// Run `io` with `stream` non-blocking, then make it block again (the mode a
/// lane serves in). `O_NONBLOCK`, not `MSG_DONTWAIT`: Darwin's `send` ignores
/// the flag on an `AF_UNIX` socket and blocks (measured on Darwin 25.6).
#[cfg(unix)]
fn without_waiting<T>(stream: &CtlStream, io: impl FnOnce() -> T) -> std::io::Result<T> {
    stream.set_nonblocking(true)?;
    let done = io();
    stream.set_nonblocking(false)?;
    Ok(done)
}

/// Every unread byte `reader` holds, taken out of it.
fn unread(reader: &mut LaneReader<'_>) -> Vec<u8> {
    let bytes = reader.buffer().to_vec();
    reader.consume(bytes.len());
    bytes
}

/// How every lane reads a connection's requests: through one buffer that
/// starts with the bytes an earlier lane had already read past the wait that
/// moved the connection here ([`Conn::carry`]), then reads the stream.
pub(super) type LaneReader<'a> = BufReader<Carried<'a>>;

/// The source under a [`LaneReader`]: the carried bytes, then the stream.
pub(super) struct Carried<'a> {
    carry: Vec<u8>,
    stream: &'a CtlStream,
}

impl<'a> Carried<'a> {
    /// The connection's stream (a readiness wait polls it).
    pub(super) fn stream(&self) -> &'a CtlStream {
        self.stream
    }
}

impl Read for Carried<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.carry.is_empty() {
            let mut stream = self.stream;
            return stream.read(buf);
        }
        let n = self.carry.len().min(buf.len());
        buf[..n].copy_from_slice(&self.carry[..n]);
        self.carry.drain(..n);
        Ok(n)
    }
}

/// A reader over `stream` whose buffer already holds `carry` when it is made:
/// the carried bytes are the next ones any read sees, and — being in the
/// buffer, not in the socket — they count wherever a lane asks whether
/// anything is buffered (a connection that has them never parks, and a
/// readiness wait never sleeps on the socket in front of them).
pub(super) fn lane_reader(stream: &CtlStream, carry: Vec<u8>) -> LaneReader<'_> {
    let carried = carry.len();
    let mut reader = BufReader::with_capacity(carried.max(READ_BUFFER), Carried { carry, stream });
    if carried > 0 {
        // The buffer is at least as large as the carry, so this one fill reads
        // the carry alone: never the socket, and it cannot fail.
        let _ = reader.fill_buf();
        debug_assert_eq!(
            reader.buffer().len(),
            carried,
            "the carry fills the buffer first"
        );
    }
    reader
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

/// One slot of a counted capacity, given back on drop: an open connection
/// (taken at admission) or a parked one (taken when it parks, given back when
/// a request lane takes it again or it closes).
struct Slot(Arc<AtomicUsize>);

impl Slot {
    /// Take one slot of `count` below `cap`, or none.
    fn take(count: &Arc<AtomicUsize>, cap: usize) -> Option<Self> {
        count
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < cap).then_some(n + 1)
            })
            .ok()?;
        Some(Self(Arc::clone(count)))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let previous = self.0.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "a capacity slot was given back twice");
    }
}

/// One connection between placements.
pub(super) struct Conn {
    stream: CtlStream,
    /// `None` until the handshake on its first lane.
    scope: Option<Scope>,
    /// Who presented its token (phase 3A, the session log's `by=`), from the
    /// handshake.
    principal: Principal,
    tenure: ConnTenure,
    /// Bytes read from the peer and not served yet, read first wherever the
    /// connection lands ([`lane_reader`]): the requests a request lane had read
    /// past the wait it moved to a wait lane, or what has come of the next
    /// request while it is not whole (the parker adds to it as more arrives).
    carry: Vec<u8>,
    /// The part of its last answer the peer has not taken yet.
    reply: Outbox,
    /// The frame header at the head of [`Self::carry`] has been acted on
    /// ([`LaneService::frame_header`]): only its whole body wakes it now.
    header_done: bool,
    /// Until the handshake: when the parker drops the connection if its first
    /// line is still not whole (`AUTH_READ_TIMEOUT` from admission).
    expires: Option<Instant>,
    /// Held from parking until a request lane takes it back.
    parked: Option<Slot>,
    /// Held from admission until the connection ends (or flips to push). Last,
    /// so a new peer is admitted only after this one's claims are released.
    _open: Slot,
}

impl Conn {
    /// End the connection: an explicit shutdown first (a vanished macOS
    /// AF_UNIX peer can otherwise linger in the kernel), then the tenure's
    /// release and the open slot as the value drops.
    ///
    /// The shutdown is a HANG-UP of each half on its own
    /// ([`aterm_uds::hangup::hang_up`]). A lane closes on its client's EOF,
    /// and a client that half-closed after its request (`printf … | nc -U`)
    /// is still reading; Darwin refuses a both-ways `shutdown` of a socket in
    /// that state, shutting nothing, so such a client read its EOF only when
    /// the last copy of this end closed — and a child any thread forked holds
    /// one until it execs.
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
    /// free again, and its socket blocks again (a parked one does not). `false`
    /// when it cannot be made to: the lane closes it.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ControlLaneTenure",
            action = "Pick",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    fn take_back(&mut self) -> bool {
        self.parked = None;
        #[cfg(unix)]
        {
            self.stream.set_nonblocking(false).is_ok()
        }
        #[cfg(not(unix))]
        {
            true
        }
    }
}

/// The request lanes' work: a freshly accepted connection (holding the open
/// slot its admission took), or a parked one that turned readable.
enum Job {
    Fresh(CtlStream, Slot),
    #[cfg_attr(
        not(unix),
        expect(dead_code, reason = "only the unix parker resumes a connection")
    )]
    Resume(Conn),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lane {
    Rpc,
    Wait,
}

/// Why a lane let go of a connection.
enum Here {
    Closed,
    Subscribe(String, Scope),
    /// Between requests — idle, or waiting on its peer for the rest of its
    /// next request or to take the rest of its answer: park it.
    #[cfg_attr(not(unix), allow(dead_code))]
    Idle,
    /// A wait read on a request lane: move it, with what the lane had read past
    /// it in [`Conn::carry`].
    Wait(String),
}

/// The wait lanes: grown one thread at a time up to `max`, and kept.
struct WaitLanes {
    max: usize,
    state: Mutex<WaitState>,
    ready: Condvar,
}

#[derive(Default)]
struct WaitState {
    queue: VecDeque<(Conn, String)>,
    idle: usize,
    threads: usize,
    running: usize,
}

#[cfg(unix)]
struct Parker {
    parked: Arc<AtomicUsize>,
    /// Connections handed to the parker and not yet polled — `None` once it has
    /// STOPPED, which a lane parking a connection reads under this lock (so
    /// nothing is handed over after the stop's last look) and answers by
    /// keeping its connection.
    inbox: Mutex<Option<Vec<Conn>>>,
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

/// The whole lane set of one control socket.
pub(super) struct Lanes {
    limits: LaneLimits,
    rpc: BoundedDispatch<Job>,
    waits: WaitLanes,
    /// Request lanes serving a wait right now (bounded by `limits.inline`).
    inline: Arc<AtomicUsize>,
    open: Arc<AtomicUsize>,
    #[cfg(unix)]
    parker: OnceLock<Arc<Parker>>,
    service: OnceLock<Arc<dyn LaneService>>,
    /// The scheduling tests' own [`super::FinalReplies`]
    /// ([`Self::count_final_replies_on`]); the instance's otherwise.
    #[cfg(test)]
    final_replies: OnceLock<&'static super::FinalReplies>,
    /// How long a fresh connection has to make its handshake line whole
    /// (milliseconds): `AUTH_READ_TIMEOUT`, short in the tests.
    handshake_ms: AtomicU64,
    /// The negative control's switch ([`Self::keep_pipelined_waits`]).
    #[cfg(test)]
    pipelined_waits_stay: std::sync::atomic::AtomicBool,
}

/// What the lanes hold right now (the Tier-1 bind's projection).
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Census {
    /// Request-lane work, queued plus running (fresh admission reads this).
    pub(super) rpc: usize,
    /// Parked connections plus resumes no lane has taken yet.
    pub(super) parked: usize,
    /// Wait-lane work, queued plus running.
    pub(super) waits: usize,
    pub(super) wait_threads: usize,
    /// Request lanes serving a wait.
    pub(super) inline: usize,
    /// Whether idle connections park (a parker runs, and has not stopped).
    pub(super) parking: bool,
    /// Connections open (admitted, not yet ended or flipped to push).
    pub(super) open: usize,
}

impl Lanes {
    pub(super) fn new(limits: LaneLimits) -> Arc<Self> {
        Arc::new(Self {
            limits,
            // Room for every request lane's fresh admission plus every resume a
            // parked connection can put in the queue.
            rpc: BoundedDispatch::with_resume_room(limits.rpc, limits.open),
            waits: WaitLanes {
                max: limits.wait,
                state: Mutex::new(WaitState::default()),
                ready: Condvar::new(),
            },
            inline: Arc::default(),
            open: Arc::default(),
            #[cfg(unix)]
            parker: OnceLock::new(),
            service: OnceLock::new(),
            #[cfg(test)]
            final_replies: OnceLock::new(),
            handshake_ms: AtomicU64::new(
                u64::try_from(AUTH_READ_TIMEOUT.as_millis()).unwrap_or(u64::MAX),
            ),
            #[cfg(test)]
            pipelined_waits_stay: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Give fresh connections `lifetime` to make their handshake line whole
    /// (the tests' short one).
    #[cfg(test)]
    pub(super) fn set_handshake_lifetime(&self, lifetime: Duration) {
        let ms = u64::try_from(lifetime.as_millis()).unwrap_or(u64::MAX);
        self.handshake_ms.store(ms, Ordering::Release);
    }

    /// Count the answers these lanes owe at exit on `replies` rather than the
    /// instance's, so a test reads its own lanes' count alone.
    #[cfg(test)]
    pub(super) fn count_final_replies_on(&self, replies: &'static super::FinalReplies) {
        assert!(
            self.final_replies.set(replies).is_ok(),
            "set once, before any request"
        );
    }

    /// Where these lanes count the answers the exit waits for.
    fn final_replies(&self) -> &'static super::FinalReplies {
        #[cfg(test)]
        if let Some(replies) = self.final_replies.get() {
            return replies;
        }
        &super::FINAL_REPLIES
    }

    fn handshake_lifetime(&self) -> Duration {
        Duration::from_millis(self.handshake_ms.load(Ordering::Acquire))
    }

    /// Start the request lanes and the parker. Returns the number of request
    /// lanes running; admission is published for exactly those. If the parker
    /// cannot start, idle connections stay on their lanes.
    pub(super) fn start(self: &Arc<Self>) -> usize {
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
        started
    }

    /// Publish what the lanes serve with. No connection is admitted before.
    pub(super) fn publish(&self, service: Arc<dyn LaneService>) {
        let _ = self.service.set(service);
    }

    /// Admit a freshly accepted connection onto a request lane, or hand it back
    /// (for the listener's busy reply) when every request lane is taken by work
    /// or the open-connection bound is reached.
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
        let cap = if self.parker_runs() {
            self.limits.open
        } else {
            usize::MAX
        };
        let Some(open) = Slot::take(&self.open, cap) else {
            return Err(stream);
        };
        self.rpc
            .try_submit(Job::Fresh(stream, open))
            .map_err(|job| match job {
                Job::Fresh(stream, _open) => stream,
                Job::Resume(_) => unreachable!("admission submits only fresh connections"),
            })
    }

    #[cfg(test)]
    pub(super) fn census(&self) -> Census {
        let waits = self
            .waits
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        Census {
            rpc: self.rpc.outstanding(),
            parked: self.parked(),
            waits: waits.queue.len() + waits.running,
            wait_threads: waits.threads,
            inline: self.inline.load(Ordering::Acquire),
            parking: self.parker_runs(),
            open: self.open.load(Ordering::Acquire),
        }
    }

    #[cfg(all(test, unix))]
    fn parked(&self) -> usize {
        self.parker
            .get()
            .map_or(0, |p| p.parked.load(Ordering::Acquire))
    }

    /// Raise `count` panics in the parker's loop, one per pass (the tests'
    /// injected failure; see [`Parker::faults`]).
    #[cfg(all(test, unix))]
    pub(super) fn inject_parker_panics(&self, count: usize) {
        if let Some(parker) = self.parker.get() {
            parker.faults.store(count, Ordering::Release);
        }
    }

    #[cfg(all(test, not(unix)))]
    fn parked(&self) -> usize {
        0
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
                    let conn = Conn {
                        stream,
                        scope: None,
                        principal: Principal::Instance,
                        tenure: ConnTenure {
                            id: super::NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed),
                            claimed: false,
                            service: Arc::clone(self.service()),
                        },
                        carry: Vec::new(),
                        reply: Outbox::default(),
                        header_done: false,
                        expires: Some(Instant::now() + self.handshake_lifetime()),
                        parked: None,
                        _open: open,
                    };
                    self.drive(conn, None, Lane::Rpc);
                }
                Job::Resume(mut conn) => {
                    if conn.take_back() {
                        self.drive(conn, None, Lane::Rpc);
                    } else {
                        conn.close();
                    }
                }
            }) {
                aterm_log::warn!("control worker recovered after a connection panic");
            }
        }
    }

    fn wait_lane(self: &Arc<Self>) {
        let mut state = self
            .waits
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some((conn, line)) = state.queue.pop_front() {
                state.running += 1;
                drop(state);
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.drive(conn, Some(line), Lane::Wait);
                }))
                .is_err()
                {
                    aterm_log::warn!("control wait lane recovered after a connection panic");
                }
                state = self
                    .waits
                    .state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                state.running -= 1;
                continue;
            }
            state.idle += 1;
            state = self
                .waits
                .ready
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
            state.idle -= 1;
        }
    }

    /// Hand `conn` and the wait it just read to a wait lane: an idle one, or a
    /// new one while the cap allows. Never queues behind running waits — a
    /// wait no lane can take at once comes back, to be served where it is
    /// (boxed: it is built only when no wait lane can take the wait).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ControlLaneTenure",
            action = "Defer",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    fn defer(self: &Arc<Self>, conn: Conn, line: String) -> Result<(), Box<(Conn, String)>> {
        let mut state = self
            .waits
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if state.idle <= state.queue.len() {
            if state.threads >= self.waits.max {
                return Err(Box::new((conn, line)));
            }
            let lanes = Arc::clone(self);
            let name = format!("aterm-control-wait-{}", state.threads);
            match crate::qos::spawn_lane_thread(name, crate::qos::Role::Responsive, move || {
                lanes.wait_lane();
            }) {
                Ok(_) => state.threads += 1,
                Err(error) => {
                    aterm_log::warn!("control wait lane could not start: {error}");
                    return Err(Box::new((conn, line)));
                }
            }
        }
        state.queue.push_back((conn, line));
        self.waits.ready.notify_one();
        Ok(())
    }

    /// Carry one connection until it leaves this lane: closed, flipped to push,
    /// parked, or moved to a wait lane. Every fallback keeps it here.
    fn drive(self: &Arc<Self>, mut conn: Conn, mut line: Option<String>, lane: Lane) {
        let mut may_park = true;
        let mut may_defer = true;
        loop {
            match self.serve_here(&mut conn, line.take(), lane, may_park, may_defer) {
                Here::Closed => {
                    conn.close();
                    return;
                }
                Here::Subscribe(line, scope) => {
                    let Conn { stream, tenure, .. } = conn;
                    // A push stream reads no more requests: what it bound ends here.
                    drop(tenure);
                    let service = Arc::clone(self.service());
                    service.subscribe(line, scope, stream);
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
                        line = Some(wait);
                        may_defer = false;
                    }
                },
            }
        }
    }

    /// Serve `conn` on this lane until it must leave it (see [`Here`]).
    ///
    /// While a parker can take the connection (`may_park`, or once a request
    /// has been served here), the lane never waits on its peer: it delivers
    /// what the peer takes of an answer now and parks the rest, and it serves
    /// only a request that is whole, parking a connection whose next one is
    /// not. Once a park was refused (a parker that stopped), the next request
    /// is read and answered here, waiting, as without a parker.
    fn serve_here(
        &self,
        conn: &mut Conn,
        mut pending: Option<String>,
        lane: Lane,
        may_park: bool,
        mut may_defer: bool,
    ) -> Here {
        let service = Arc::clone(self.service());
        let Conn {
            stream,
            scope,
            principal,
            tenure,
            carry,
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
        let mut reader = lane_reader(stream, std::mem::take(carry));
        let mut served = false;
        let scope = match *scope {
            Some(scope) => scope,
            None => {
                // The handshake is read once its line is whole; until then the
                // parker holds the connection, for the rest of its lifetime.
                if may_park && self.parker_runs() {
                    match gather_here(stream, &mut reader, |buffered| {
                        held(buffered, false) == Held::Whole
                    }) {
                        // Whole, or all the peer will send: read it either way.
                        Gathered::Enough | Gathered::Ended => {}
                        Gathered::Short => {
                            *carry = unread(&mut reader);
                            return Here::Idle;
                        }
                        Gathered::Broken => return Here::Closed,
                    }
                }
                let Some((authenticated, presented, inline)) =
                    service.authenticate(stream, &mut reader)
                else {
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
                    let mut head = verb.into_bytes();
                    head.push(b'\n');
                    head.extend_from_slice(&unread(&mut reader));
                    reader = lane_reader(stream, head);
                }
                authenticated
            }
        };
        // The session log names this connection's Owner requests by who
        // presented its token, for as long as this lane serves it.
        let _principal = super::ServingPrincipal::enter(*principal);
        loop {
            let parking = (may_park || served) && self.parker_runs();
            // The last answer goes out first, and nothing more is read until the
            // peer has taken it all: what it does not take now, the parker
            // delivers as it makes room.
            if reply.owed() {
                match deliver_here(stream, reply, parking) {
                    Ok(true) => {}
                    Ok(false) => {
                        *carry = unread(&mut reader);
                        return Here::Idle;
                    }
                    Err(_) => return Here::Closed,
                }
            }
            if reply.then_close {
                return Here::Closed;
            }
            let line = match pending.take() {
                Some(line) => line,
                None if parking => {
                    match next_request(stream, &mut reader, scope, header_done, reply, &*service) {
                        Next::Line(line) => line,
                        Next::Park => {
                            *carry = unread(&mut reader);
                            return Here::Idle;
                        }
                        Next::Refused => {
                            reply.then_close = true;
                            continue;
                        }
                        Next::Closed => return Here::Closed,
                    }
                }
                None => match read_authenticated_request_line(&mut reader) {
                    Some(line) => line,
                    None => return Here::Closed,
                },
            };
            // A wait read on a request lane moves to a wait lane when it can —
            // with whatever this lane had read past it, so a request pipelined
            // behind it is served after it, where it lands. When no wait lane can
            // take it, it is served here only within the budget; past it, the
            // busy reply — nothing served, the connection kept — so waits never
            // hold more than `limits.inline` request lanes.
            let mut inline = None;
            if lane == Lane::Rpc && service.is_wait(&line, scope) {
                if may_defer && self.limits.wait > 0 && self.moves_with(&reader) {
                    *carry = reader.buffer().to_vec();
                    return Here::Wait(line);
                }
                inline = self.take_inline();
                if inline.is_none() {
                    refuse_wait(&mut reply.bytes);
                    // A refused frame's body was never read: left in the
                    // stream, the next turn would serve it as requests under
                    // this connection's scope. Closed once the busy line is
                    // out, as a frame refused on its header is.
                    reply.then_close = announced_body(&line) > 0;
                    may_defer = true;
                    served = true;
                    continue;
                }
            }
            may_defer = true;
            // Owed from BEFORE the dispatch (the action may end the process) until
            // the answer is delivered ([`Outbox::final_reply`]).
            let final_reply = self.final_replies().enter(&line);
            let disposition =
                service.serve_line(line, scope, stream, &mut reader, &mut reply.bytes);
            if reply.owed() {
                reply.final_reply = final_reply;
            }
            drop(inline);
            served = true;
            match disposition {
                None => {}
                // Closed once its answer is delivered.
                Some(ServeDisposition::Close) => reply.then_close = true,
                Some(ServeDisposition::Subscribe { line, scope }) => {
                    return Here::Subscribe(line, scope);
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
        Slot::take(&self.inline, self.limits.inline)
    }

    /// Whether a wait read on a request lane may move with what `reader` holds
    /// behind it: always. The scheduling tests' negative control restores the
    /// rule before the carry ([`Self::keep_pipelined_waits`]): only with
    /// nothing buffered.
    fn moves_with(&self, reader: &LaneReader<'_>) -> bool {
        #[cfg(test)]
        if self.pipelined_waits_stay.load(Ordering::Acquire) {
            return reader.buffer().is_empty();
        }
        let _ = reader;
        true
    }

    /// The negative control's rule: a wait with anything buffered behind it
    /// stays on its request lane (and so takes one of the budget's places, or
    /// is answered busy, whatever the wait lanes hold) — the lanes before the
    /// carry.
    #[cfg(test)]
    pub(super) fn keep_pipelined_waits(&self) {
        self.pipelined_waits_stay.store(true, Ordering::Release);
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

    /// Park a connection between requests: idle, or waiting on its peer for
    /// the rest of its next request or to take the rest of its last answer. Its
    /// socket stops blocking (the parker never waits on one descriptor). The
    /// open-connection bound is the parker's capacity, so a slot is always free
    /// here while the parker runs; the check stays, and a refusal keeps the
    /// connection on its lane, as before — as does a parker that has stopped
    /// (read under the inbox's lock, so no connection is handed over after its
    /// last look), or a socket that cannot be made non-blocking.
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
        let mut inbox = parker.inbox.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(waiting) = inbox.as_mut() else {
            return Err(Box::new(conn));
        };
        let Some(slot) = Slot::take(&parker.parked, self.limits.open) else {
            return Err(Box::new(conn));
        };
        if conn.stream.set_nonblocking(true).is_err() {
            return Err(Box::new(conn));
        }
        conn.parked = Some(slot);
        waiting.push(conn);
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
            parked: Arc::default(),
            inbox: Mutex::new(Some(Vec::new())),
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

    /// The parker cannot go on: from now on no connection parks (a lane keeps
    /// its idle connection, and the lanes bound the connections, as without a
    /// parker), and every connection it holds — polled, or handed over and not
    /// yet polled — is closed: its client sees a hang-up, which it rides out by
    /// dialling again, instead of a request nobody will ever read.
    #[cfg(unix)]
    fn stop_parker(&self, parker: &Parker, parked: Vec<Conn>, why: &str) {
        let arrivals = {
            let mut inbox = parker.inbox.lock().unwrap_or_else(PoisonError::into_inner);
            parker.running.store(false, Ordering::Release);
            inbox.take().unwrap_or_default()
        };
        let closed = parked.len() + arrivals.len();
        for conn in parked.into_iter().chain(arrivals) {
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
    /// `POLLIN` for the rest, and no longer than until the first handshake runs
    /// out. Returns only when it cannot poll, saying why; every pass it
    /// completes zeroes `panics`.
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
        take_arrivals(parker, parked);
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
            let timeout = until_a_handshake_expires(parked, Instant::now());
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
            // handshakes that ran out. Downward, so a `swap_remove` only moves
            // an entry already looked at.
            let now = Instant::now();
            for index in (0..parked.len()).rev() {
                let events = fds[index + 1].revents;
                if events == 0 && !parked[index].expires.is_some_and(|at| at <= now) {
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
                take_arrivals(parker, parked);
            }
            *panics = 0;
        }
    }

    /// Act on what woke a parked connection (`events`, or its handshake's time
    /// running out at `now`); `Some` hands it back to stay parked.
    #[cfg(unix)]
    fn wake(&self, mut conn: Conn, events: libc::c_short, now: Instant) -> Option<Conn> {
        let step = if events & libc::POLLNVAL != 0 {
            Step::Close
        } else if events == 0 {
            Step::Stay
        } else if conn.reply.owed() {
            conn.deliver_parked()
        } else {
            conn.gather_parked()
        };
        match step {
            Step::Serve => {
                self.resume(conn);
                None
            }
            Step::Stay if !conn.expires.is_some_and(|at| at <= now) => Some(conn),
            // Closed: its peer hung up, its connection failed, it asked to be
            // once answered, or its handshake did not come whole in time.
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
        if let Err(job) = self.rpc.submit_resumed(Job::Resume(conn)) {
            // Unreachable while the open bound sizes the queue; close rather
            // than silently strand a live connection if that ever breaks.
            aterm_log::warn!("control parker: resume refused, closing");
            if let Job::Resume(conn) = job {
                conn.close();
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
    /// A frame's header was refused, its answer in the outbox: close.
    Refused,
    /// Nothing more will come, or the connection failed.
    Closed,
}

/// The connection's next request, without waiting for its peer: served from
/// what `reader` holds once it is whole, with what the peer has already sent
/// gathered in first. A frame's header is acted on as soon as it is whole
/// ([`LaneService::frame_header`]), before its body is waited for.
fn next_request<'a>(
    stream: &'a CtlStream,
    reader: &mut LaneReader<'a>,
    scope: Scope,
    header_done: &mut bool,
    reply: &mut Outbox,
    service: &dyn LaneService,
) -> Next {
    loop {
        match held(reader.buffer(), true) {
            Held::Whole => {
                *header_done = false;
                return read_authenticated_request_line(reader).map_or(Next::Closed, Next::Line);
            }
            Held::Header(end) if !*header_done => {
                let line = decode_request_line(reader.buffer()[..end].to_vec());
                if !service.frame_header(&line, scope, &mut reply.bytes) {
                    return Next::Refused;
                }
                *header_done = true;
            }
            Held::Header(_) | Held::Partial => {}
        }
        let done = *header_done;
        match gather_here(stream, reader, |buffered| servable(buffered, true, done)) {
            Gathered::Enough => {}
            Gathered::Short => return Next::Park,
            // All the peer will send is here: what it left is served as it is (a
            // last line with no newline; a frame cut short closes), and an
            // empty end is the end.
            Gathered::Ended if reader.buffer().is_empty() => return Next::Closed,
            Gathered::Ended => {
                *header_done = false;
                return read_authenticated_request_line(reader).map_or(Next::Closed, Next::Line);
            }
            Gathered::Broken => return Next::Closed,
        }
    }
}

/// [`gather`] into what `reader` holds, on a lane: the socket is non-blocking
/// for the call only.
#[cfg(unix)]
fn gather_here<'a>(
    stream: &'a CtlStream,
    reader: &mut LaneReader<'a>,
    enough: impl Fn(&[u8]) -> bool,
) -> Gathered {
    let mut bytes = unread(reader);
    let gathered =
        without_waiting(stream, || gather(stream, &mut bytes, enough)).unwrap_or(Gathered::Broken);
    *reader = lane_reader(stream, bytes);
    gathered
}

/// No parker runs off unix, so no lane gathers without waiting.
#[cfg(not(unix))]
fn gather_here(
    _stream: &CtlStream,
    _reader: &mut LaneReader<'_>,
    _enough: impl Fn(&[u8]) -> bool,
) -> Gathered {
    unreachable!("a lane gathers only while a parker runs")
}

/// Deliver the outbox from a lane: what the peer takes now when the parker can
/// have the rest (`parking`), else all of it, waiting.
fn deliver_here(stream: &CtlStream, reply: &mut Outbox, parking: bool) -> std::io::Result<bool> {
    #[cfg(unix)]
    if parking {
        return without_waiting(stream, || reply.deliver(stream, true))?;
    }
    let _ = parking;
    reply.deliver(stream, false)
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
    /// Deliver what the peer has room for of the answer it is owed.
    fn deliver_parked(&mut self) -> Step {
        match self.reply.deliver(&self.stream, true) {
            Ok(false) => Step::Stay,
            Ok(true) if self.reply.then_close => Step::Close,
            // Delivered: the next request may be whole already (read behind
            // the answer); otherwise the parker waits for it.
            Ok(true) if servable(&self.carry, self.scope.is_some(), self.header_done) => {
                Step::Serve
            }
            Ok(true) => Step::Stay,
            Err(_) => Step::Close,
        }
    }

    /// Read what has come of the next request.
    fn gather_parked(&mut self) -> Step {
        let (authenticated, header_done) = (self.scope.is_some(), self.header_done);
        match gather(&self.stream, &mut self.carry, |buffered| {
            servable(buffered, authenticated, header_done)
        }) {
            Gathered::Enough => Step::Serve,
            Gathered::Short => Step::Stay,
            // The peer sends no more: a lane serves what it left (and then
            // reads the end), or there is nothing left to serve.
            Gathered::Ended if !self.carry.is_empty() => Step::Serve,
            Gathered::Ended | Gathered::Broken => Step::Close,
        }
    }
}

/// How long the parker may wait before the first handshake it holds runs
/// out: a `poll(2)` timeout in milliseconds, rounded up so it never wakes
/// early; -1 (no limit) when every connection it holds has authenticated.
#[cfg(unix)]
fn until_a_handshake_expires(parked: &[Conn], now: Instant) -> libc::c_int {
    parked
        .iter()
        .filter_map(|conn| conn.expires)
        .min()
        .map_or(-1, |at| {
            let left = at.saturating_duration_since(now).as_millis();
            libc::c_int::try_from(left.saturating_add(1)).unwrap_or(libc::c_int::MAX)
        })
}

/// Move what lanes handed the parker into its polled set.
#[cfg(unix)]
fn take_arrivals(parker: &Parker, parked: &mut Vec<Conn>) {
    // `append` leaves the inbox its capacity for the next arrivals.
    if let Some(inbox) = parker
        .inbox
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_mut()
    {
        parked.append(inbox);
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

/// A wait no lane may take — every wait lane taken (a wait with requests
/// pipelined behind it moves too, carrying them), and the request lanes' wait
/// budget spent — is answered with the busy reply, at once: nothing was
/// served, and the client may send it again. The connection stays (it parks,
/// idle, like any answered one, or serves what is pipelined behind the wait);
/// only a failed delivery ends it.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ControlLaneTenure",
        action = "BusyWait",
        project = "aterm_gui::control::control_lanes::conformance::project"
    )
)]
fn refuse_wait(out: &mut Vec<u8>) {
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

#[cfg(test)]
mod outbox_delivery_tests {
    use super::{CtlStream, Outbox};
    use std::io::Read as _;
    use std::time::Duration;

    #[test]
    fn delivery_preserves_the_wire_and_erases_the_retained_reply() {
        let (server, mut peer) = CtlStream::pair().expect("private control pair");
        let hang_detector = Some(Duration::from_secs(60));
        server.set_write_timeout(hang_detector).unwrap();
        peer.set_read_timeout(hang_detector).unwrap();
        let reply = b"OK askpass secret=74657374\n";
        let mut out = Outbox::default();
        out.bytes.extend_from_slice(reply);
        let allocation = out.bytes.as_ptr();
        let capacity = out.bytes.capacity();
        assert!(
            capacity <= 8 * super::READ_BUFFER,
            "exercise retained storage"
        );

        assert!(out.deliver(&server, false).unwrap());
        let mut received = vec![0; reply.len()];
        peer.read_exact(&mut received).unwrap();
        assert_eq!(received, reply, "delivery must preserve every wire byte");
        assert!(out.bytes.is_empty());
        assert_eq!(out.sent, 0);
        assert_eq!(out.bytes.capacity(), capacity);
        assert_eq!(out.bytes.as_ptr(), allocation);
        // SAFETY: exactly this prefix was initialized above. Delivery kept the
        // allocation, and clearing a Vec<u8> leaves its bytes initialized. Read
        // only that known prefix through MaybeUninit, never unused capacity or
        // a freed allocation.
        let retained: Vec<u8> = out.bytes.spare_capacity_mut()[..reply.len()]
            .iter()
            .map(|byte| unsafe { byte.assume_init() })
            .collect();
        assert!(
            retained.iter().all(|byte| *byte == 0),
            "delivered reply remains in the reusable allocation: {retained:?}"
        );
    }
}
