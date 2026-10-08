// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The control socket's lanes: a request holds a thread, a connection does
//! not. Supervised agent tabs each hold a persistent `RelayCtl` that is idle
//! most of the time, so a connection must not pin a thread while idle.
//!
//! **Where a connection is, at any moment — exactly one of:**
//!
//! * on a request lane (`CONTROL_WORKERS` fixed threads), authenticating or
//!   serving a request. Admission counts these, queued plus running.
//! * on a WAIT lane ([`WaitLanes`], at most `CONTROL_WAIT_WORKERS` threads, grown
//!   on demand and kept): serving a request whose time is spent WAITING on an
//!   event — `await`, `ready`, `wait`, `turn`, a `post` or `inbox get @…` that
//!   parks on the bridge — or relaying the connection somewhere else (`dial
//!   <name>`, a session this instance does not host). A request lane that reads
//!   such a line hands the connection over and is free again at once.
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
//!   ([`Conn::carry`]): the one in-tree client that writes that way,
//!   `RelayClient::dial_via_local` (`dial <name>` then `version`, back to back),
//!   is one `aterm drive --dial` relay each, and while such waits stayed on their
//!   request lane the budget's four places turned the fifth relay away with most
//!   of the wait lanes free.
//! * PARKED ([`Parker`], one thread): authenticated, idle between requests,
//!   nothing buffered. One `poll(2)` over every parked descriptor; a connection
//!   that turns readable goes back to the request lanes' queue, and one whose
//!   peer hung up with nothing to read is closed right there.
//! * on the subscription pool (unchanged: a `subscribe` flips it to push).
//!
//! **Admission.** A fresh connection gets `ERR control server busy; retry`
//! when every request lane is taken by work, or when the open-connection cap
//! is reached (`control_connections_cap`: a quarter of the soft descriptor
//! limit, at most `CONTROL_CONNECTIONS_MAX`). That cap is the parker's
//! capacity, so an idle connection always has a place to park and never keeps
//! a lane (which would also starve the resumes queued behind it). The cap
//! bounds open connections, not requests, and leaves most descriptors to tabs
//! and files.
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
//! connection moves only between requests; it parks only with nothing left in
//! its read buffer (a pipelined next request already buffered is served where it
//! is); and a wait that moves takes what its lane had read past it along, served
//! first where it lands, before anything more is read from the socket. The auth
//! and uid checks (the listener's, and the handshake on the first lane). Claims
//! bound to a connection (`meta set supervisor` without `ttl=`) stay bound across
//! every move and are released when the connection ends, however it ends — the
//! release is a drop guard ([`ConnTenure`]), so a lane's panic or the parker's
//! close releases them too. The cut `RelayCtl`'s interrupter makes (a client-side
//! `shutdown`) reads, wherever the connection is, as the hang-up it always was.
//! Every queue is bounded and every thread count is capped. Without a parker (off
//! unix, one that could not start, or one that stopped) an idle connection stays
//! on its lane and the connection bound is the lanes', exactly as before.
//!
//! Derived model: `aterm_spec::derive::control_lane_tenure_model` (a running
//! parker's lanes; a stopped parker's fallback is the design it rejects, on
//! purpose); its Tier-1 bind (`control_lanes_conformance.rs`) drives these lanes
//! with real sockets.

use std::collections::VecDeque;
use std::io::{BufRead as _, BufReader, Read, Write as _};
#[cfg(unix)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};

use aterm_uds::CtlStream;

use super::{BoundedDispatch, Scope, ServeDisposition, read_authenticated_request_line};

/// What a lane serves a connection's requests with: the control server in the
/// window ([`super::ControlWorkerContext`]), a stub in the scheduling tests.
pub(super) trait LaneService: Send + Sync {
    /// Read the handshake and resolve its scope, with the verb a `TOKEN` line
    /// folded in. `None` closes the connection (the service has answered).
    fn authenticate(
        &self,
        stream: &CtlStream,
        reader: &mut LaneReader<'_>,
    ) -> Option<(Scope, Option<String>)>;
    /// Serve one authenticated request. `None` keeps the connection.
    fn serve_line(
        &self,
        line: String,
        scope: Scope,
        stream: &CtlStream,
        reader: &mut LaneReader<'_>,
    ) -> Option<ServeDisposition>;
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

/// The read buffer a lane serves a connection through (`BufReader::new`'s).
const READ_BUFFER: usize = 8 * 1024;

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
    tenure: ConnTenure,
    /// The bytes a request lane had already read past the wait it moved to a
    /// wait lane — the requests pipelined behind it — read first where it lands
    /// ([`lane_reader`]). Empty everywhere else: every lane takes it as it
    /// starts, and a connection parks only with nothing buffered.
    carry: Vec<u8>,
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
        test,
        aterm_spec::refines(
            machine = "ControlLaneTenure",
            action = "Close",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "ControlLaneTenure",
            action = "WaitClose",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "ControlLaneTenure",
            action = "InlineClose",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    fn close(self) {
        aterm_uds::hangup::hang_up(&self.stream);
    }

    /// A request lane takes a resumed connection back: its parking slot is
    /// free again.
    #[cfg_attr(
        test,
        aterm_spec::refines(
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
/// slot its admission took), or a parked one that turned readable.
enum Job {
    Fresh(CtlStream, Slot),
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
    /// Idle, nothing buffered: park it.
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
    /// lock by admission and [`Lanes::parkable`].
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
            pipelined_waits_stay: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Start the request lanes and the parker. Returns the number of request
    /// lanes running; admission is published for exactly those. If the parker
    /// cannot start, idle connections stay on their lanes.
    pub(super) fn start(self: &Arc<Self>) -> usize {
        let mut started = 0;
        for index in 0..self.limits.rpc {
            let lanes = Arc::clone(self);
            match std::thread::Builder::new()
                .name(format!("aterm-control-{index}"))
                .spawn(move || {
                    // QoS floor (qos.rs): this lane runs verb dispatch, and a
                    // read verb holds the terminal mutex while it formats. At the
                    // default band it would sit below the UI thread that takes
                    // the same mutex, leaving a descheduled holder in front of the
                    // next keystroke. Not `Interactive`: a verb storm must never
                    // outrank the UI thread.
                    crate::qos::set_self(crate::qos::Role::Responsive);
                    lanes.rpc_lane();
                }) {
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
        test,
        aterm_spec::refines(
            machine = "ControlLaneTenure",
            action = "Admit",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
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
                        tenure: ConnTenure {
                            id: super::NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed),
                            claimed: false,
                            service: Arc::clone(self.service()),
                        },
                        carry: Vec::new(),
                        parked: None,
                        _open: open,
                    };
                    self.drive(conn, None, Lane::Rpc);
                }
                Job::Resume(mut conn) => {
                    conn.take_back();
                    self.drive(conn, None, Lane::Rpc);
                }
            }) {
                aterm_log::warn!("control worker recovered after a connection panic");
            }
        }
    }

    fn wait_lane(self: &Arc<Self>) {
        crate::qos::set_self(crate::qos::Role::Responsive);
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
        test,
        aterm_spec::refines(
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
            match std::thread::Builder::new()
                .name(name)
                .spawn(move || lanes.wait_lane())
            {
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
                        // Its next request is read here, blocking; parking is
                        // tried again once that request is answered.
                        conn = back;
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
            tenure,
            carry,
            ..
        } = conn;
        let stream: &CtlStream = stream;
        let _serving = Serving::enter(tenure.id, &mut tenure.claimed);
        // A blocking verb served here gives the lane back when this caller
        // hangs up (`super::caller_hung_up`).
        let _hangup = super::ServingFd::enter(stream);
        let mut reader = lane_reader(stream, std::mem::take(carry));
        let scope = match *scope {
            Some(scope) => scope,
            None => {
                let Some((authenticated, inline)) = service.authenticate(stream, &mut reader)
                else {
                    return Here::Closed;
                };
                *scope = Some(authenticated);
                // A bare `TOKEN` line is only an acknowledgement.
                pending = inline.filter(|verb| !verb.is_empty());
                authenticated
            }
        };
        let mut served = false;
        loop {
            let line = match pending.take() {
                Some(line) => line,
                None => {
                    if (may_park || served) && reader.buffer().is_empty() && self.parkable(stream) {
                        return Here::Idle;
                    }
                    match read_authenticated_request_line(&mut reader) {
                        Some(line) => line,
                        None => return Here::Closed,
                    }
                }
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
                    if refuse_wait(stream).is_err() {
                        return Here::Closed;
                    }
                    may_defer = true;
                    served = true;
                    continue;
                }
            }
            may_defer = true;
            let disposition = service.serve_line(line, scope, stream, &mut reader);
            drop(inline);
            match disposition {
                None => served = true,
                Some(ServeDisposition::Close) => return Here::Closed,
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
        test,
        aterm_spec::refines(
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

    /// Whether an idle connection could park now: a parker runs, and the
    /// connection has nothing to read yet (parking it would only bounce it
    /// straight back).
    #[cfg(unix)]
    fn parkable(&self, stream: &CtlStream) -> bool {
        self.parker_runs() && !readable_now(stream)
    }

    #[cfg(not(unix))]
    fn parkable(&self, _stream: &CtlStream) -> bool {
        false
    }

    #[cfg(not(unix))]
    fn park(&self, conn: Conn) -> Result<(), Conn> {
        Err(conn)
    }

    /// Park an idle connection. The open-connection bound is the parker's
    /// capacity, so a slot is always free here while the parker runs; the check
    /// stays, and a refusal keeps the connection on its lane, as before — as
    /// does a parker that has stopped (read under the inbox's lock, so no
    /// connection is handed over after its last look).
    #[cfg(unix)]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "ControlLaneTenure",
            action = "Finish",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "ControlLaneTenure",
            action = "WaitFinish",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "ControlLaneTenure",
            action = "InlineFinish",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    fn park(&self, mut conn: Conn) -> Result<(), Conn> {
        let Some(parker) = self.parker.get() else {
            return Err(conn);
        };
        let mut inbox = parker.inbox.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(waiting) = inbox.as_mut() else {
            return Err(conn);
        };
        let Some(slot) = Slot::take(&parker.parked, self.limits.open) else {
            return Err(conn);
        };
        debug_assert!(
            conn.carry.is_empty(),
            "a connection parks with nothing buffered"
        );
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
        match std::thread::Builder::new()
            .name("aterm-control-parker".to_string())
            .spawn(move || {
                // On the latency path of every parked driver's next request.
                crate::qos::set_self(crate::qos::Role::Responsive);
                lanes.parker_thread(&polled, &listen);
            }) {
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
    /// restart. Returns only when it cannot poll, saying why; every pass it
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
                events: libc::POLLIN,
                revents: 0,
            }));
            let Ok(count) = libc::nfds_t::try_from(fds.len()) else {
                return format!("{} descriptors are too many to poll", fds.len());
            };
            // SAFETY: `fds` is an initialized, exclusively borrowed array of
            // `count` pollfds, and every descriptor in it is owned by `listen`
            // or by a `Conn` in `parked`, both alive across the call. -1 waits
            // until a descriptor is ready or a signal interrupts.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), count, -1) };
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
            // Readiness of the connections this poll watched. Downward, so a
            // `swap_remove` only moves an entry already looked at.
            for index in (0..parked.len()).rev() {
                let events = fds[index + 1].revents;
                if events == 0 {
                    continue;
                }
                let conn = parked.swap_remove(index);
                // A panic here is this connection's alone: the unwind closes
                // it, and the parker goes on with the rest.
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.wake(conn, events)
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

    /// Act on what woke a parked connection; `Some` hands it back to stay
    /// parked.
    #[cfg(unix)]
    fn wake(&self, conn: Conn, events: libc::c_short) -> Option<Conn> {
        match wake_kind(&conn.stream, events) {
            Wake::Request => {
                self.resume(conn);
                None
            }
            Wake::HungUp => {
                close_parked(conn);
                None
            }
            Wake::Spurious => Some(conn),
        }
    }

    /// A parked connection turned readable: back to the request lanes' queue,
    /// behind whatever runs, holding its parking slot until a lane takes it.
    #[cfg(unix)]
    #[cfg_attr(
        test,
        aterm_spec::refines(
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
/// only a failed write ends it.
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "ControlLaneTenure",
        action = "BusyWait",
        project = "aterm_gui::control::control_lanes::conformance::project"
    )
)]
fn refuse_wait(stream: &CtlStream) -> std::io::Result<()> {
    let mut writer = stream;
    writer.write_all(aterm_types::control_verbs::CONTROL_BUSY_LINE.as_bytes())?;
    writer.flush()
}

/// A parked connection's peer hung up with nothing left to read: closed where
/// it is, its tenure (and any claim bound to it) ended with it.
#[cfg(unix)]
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "ControlLaneTenure",
        action = "ParkedClose",
        project = "aterm_gui::control::control_lanes::conformance::project"
    )
)]
fn close_parked(conn: Conn) {
    conn.close();
}

/// What woke a parked connection.
#[cfg(unix)]
enum Wake {
    /// Bytes to read (maybe with a hang-up behind them): serve them.
    Request,
    /// The peer is gone and there is nothing to read.
    HungUp,
    /// Nothing after all: stay parked.
    Spurious,
}

#[cfg(unix)]
fn wake_kind(stream: &CtlStream, events: libc::c_short) -> Wake {
    use std::os::fd::AsRawFd;

    if events & libc::POLLNVAL != 0 {
        return Wake::HungUp;
    }
    let mut byte = 0u8;
    // SAFETY: `stream` is borrowed across the call and `byte` is one writable
    // byte. MSG_PEEK leaves the byte for the lane that reads the request.
    let peeked = unsafe {
        libc::recv(
            stream.as_raw_fd(),
            std::ptr::addr_of_mut!(byte).cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    match peeked {
        n if n > 0 => Wake::Request,
        0 => Wake::HungUp,
        _ => match std::io::Error::last_os_error().kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted => Wake::Spurious,
            _ => Wake::HungUp,
        },
    }
}

/// Whether the connection already has something to read (or has hung up), so
/// parking it would only bounce it straight back.
#[cfg(unix)]
fn readable_now(stream: &CtlStream) -> bool {
    use std::os::fd::AsRawFd;

    let mut fd = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one initialized pollfd over a descriptor `stream` keeps open;
    // a zero timeout never blocks.
    let ready = unsafe { libc::poll(&mut fd, 1, 0) };
    ready != 0
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
