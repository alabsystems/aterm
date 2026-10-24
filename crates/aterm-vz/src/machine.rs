// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `Machine` port (`docs/DESIGN-vz-guest-2026-10-23.md` §2, §5): what the helper's core
//! asks of the VM. Its VZ arm is `aterm-vz-host`'s own (macOS only), over `aterm-objc` and a
//! `dlopen`ed Virtualization.framework; its Fake arm ([`fake`]) is the tests', and loads
//! nothing.
//!
//! A call returns once the framework has taken it; what happens after (the VM started, a
//! console line, the agent's watch connection, a stop) comes back as an [`Event`] through the
//! [`Sink`] `boot` was handed, from whatever thread or queue the framework answered on.

use std::any::Any;
use std::os::fd::{BorrowedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use crate::plan::{Limits, Plan, ShareEntry};

/// One vsock connection: its descriptor, and the object that owns it. A
/// `VZVirtioSocketConnection` closes its descriptor when it is destroyed (fact 31), so the
/// descriptor is borrowed from the object for exactly as long as the object lives.
pub struct Conn {
    fd: RawFd,
    port: u32,
    object: Box<dyn Any + Send>,
}

impl std::fmt::Debug for Conn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Conn")
            .field("fd", &self.fd)
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl Conn {
    /// The connection to `port` whose descriptor `fd` `object` owns.
    ///
    /// # Safety
    /// `fd` must stay open, and be closed by nothing else, for as long as `object` lives.
    pub unsafe fn new(fd: RawFd, port: u32, object: Box<dyn Any + Send>) -> Self {
        Self { fd, port, object }
    }

    /// The descriptor, borrowed from its object.
    #[must_use]
    pub fn fd(&self) -> BorrowedFd<'_> {
        // SAFETY: `Conn::new`'s contract: `object`, which `self` owns, keeps `fd` open.
        unsafe { BorrowedFd::borrow_raw(self.fd) }
    }

    /// The guest port.
    #[must_use]
    pub const fn port(&self) -> u32 {
        self.port
    }

    /// A stream on a duplicate of the descriptor: the helper's own reader and writer of the
    /// connection, which the object's close does not end.
    ///
    /// # Errors
    /// The descriptor could not be duplicated.
    pub fn stream(&self) -> std::io::Result<UnixStream> {
        Ok(UnixStream::from(self.fd().try_clone_to_owned()?))
    }

    /// The object, for an arm that must hand it back to its own queue.
    #[must_use]
    pub fn into_object(self) -> Box<dyn Any + Send> {
        self.object
    }
}

/// What the VM did, on the channel `boot` was handed.
#[derive(Debug)]
pub enum Event {
    /// `startWithCompletionHandler:` answered without an error.
    Started,
    /// It answered with this error (its domain, code and description).
    StartFailed(String),
    /// Bytes the guest wrote to hvc0.
    Console(Vec<u8>),
    /// The agent dialed the host's watch listener (port 1025).
    Watch(Conn),
    /// The guest stopped itself (`guestDidStopVirtualMachine:`).
    GuestStopped,
    /// The VM stopped with an error (`virtualMachine:didStopWithError:`).
    StoppedWithError(String),
    /// A hard stop (`stopWithCompletionHandler:`) finished: with an error, or none.
    HardStopped(Result<(), String>),
}

/// Where a machine's [`Event`]s go: callable from any thread.
#[derive(Clone)]
pub struct Sink(Arc<dyn Fn(Event) + Send + Sync>);

impl std::fmt::Debug for Sink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sink")
    }
}

impl Sink {
    /// A sink that hands each event to `f`.
    pub fn new(f: impl Fn(Event) + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }

    /// Hand `event` on.
    pub fn send(&self, event: Event) {
        (self.0)(event);
    }
}

/// The VM, as the helper's core drives it.
pub trait Machine: Send {
    /// What the framework and this host allow (`maximumAllowedCPUCount`,
    /// `maximumAllowedMemorySize`, the host's cores and memory), read at `Boot`.
    ///
    /// # Errors
    /// The framework is not there, or says this Mac cannot run a VM (`isSupported`).
    fn limits(&mut self) -> Result<Limits, String>;

    /// Build the VM for `plan` under `limits`, validate it, set the watch listener on port
    /// 1025, and start it; `events` hears the rest.
    ///
    /// # Errors
    /// The configuration did not build or validate (its error, which names a missing
    /// entitlement, fact 33).
    fn boot(&mut self, plan: &Plan, limits: &Limits, events: Sink) -> Result<(), String>;

    /// A connection to the guest's `port`.
    ///
    /// # Errors
    /// The guest refused it (no listener: `ECONNRESET`, fact 41), or the VM is not running.
    fn connect(&mut self, port: u32) -> Result<Conn, String>;

    /// Assign `entries` to the running `aterm` share (fact 42).
    ///
    /// # Errors
    /// The share could not be built or set (an `NSException` is contained).
    fn share(&mut self, entries: &[ShareEntry]) -> Result<(), String>;

    /// Ask the guest to stop (`requestStopWithError:`).
    ///
    /// # Errors
    /// The framework refused the request.
    fn request_stop(&mut self) -> Result<(), String>;

    /// Stop the VM now (`stopWithCompletionHandler:`); [`Event::HardStopped`] follows.
    ///
    /// # Errors
    /// The framework refused the call.
    fn hard_stop(&mut self) -> Result<(), String>;
}

/// The Fake arm: a scripted VM that loads nothing, for the tests.
#[cfg(any(test, debug_assertions))]
pub mod fake {
    use std::os::fd::{AsRawFd as _, OwnedFd};
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::{Conn, Event, Machine, Sink};
    use crate::plan::{Limits, Plan, ShareEntry};

    /// One call the core made.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Call {
        /// [`Machine::limits`].
        Limits,
        /// [`Machine::boot`], with the plan.
        Boot(Box<Plan>),
        /// [`Machine::connect`], with the port.
        Connect(u32),
        /// [`Machine::share`], with the entries.
        Share(Vec<ShareEntry>),
        /// [`Machine::request_stop`].
        RequestStop,
        /// [`Machine::hard_stop`].
        HardStop,
    }

    /// What the Fake answers.
    #[derive(Clone, Debug)]
    pub struct Script {
        /// [`Machine::limits`]' answer.
        pub limits: Result<Limits, String>,
        /// [`Machine::boot`]'s answer.
        pub boot: Result<(), String>,
        /// Events sent at boot, in order, when it succeeds ([`Event::Started`], or the
        /// framework's refusal).
        pub on_boot: Vec<FakeEvent>,
        /// Whether a hard stop sends [`Event::HardStopped`].
        pub hard_stop_completes: bool,
        /// [`Machine::connect`]'s refusal, when it refuses.
        pub connect_refusal: Option<String>,
        /// [`Machine::share`]'s refusal, when it refuses.
        pub share_refusal: Option<String>,
        /// A guest that powers off when asked: `requestStopWithError:`, or `Poweroff` on a
        /// command connection, is followed by [`Event::GuestStopped`].
        pub guest_obeys: bool,
    }

    /// An [`Event`] the script sends at boot.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum FakeEvent {
        /// [`Event::Started`].
        Started,
        /// [`Event::StartFailed`].
        StartFailed(String),
        /// [`Event::Console`].
        Console(Vec<u8>),
        /// The agent dials the watch listener ([`Event::Watch`]) and says hello with this
        /// hash and the key it read from the plan's key disk, then `Ready` when `ready`; its
        /// end is [`Seen::watch_end`].
        Agent {
            /// The hash its `Hello` names.
            agent_sha256: [u8; 32],
            /// Whether `Ready` follows.
            ready: bool,
        },
        /// A guest process that is not root dials the watch listener: with `speaks`, it has
        /// written a hello naming this hash with a key it guessed, then `Ready` and a
        /// `Beat`; else nothing. Its end joins [`Seen::forger_ends`].
        Forger {
            /// The hash its forged `Hello` names (the plan's, to pass that check).
            agent_sha256: [u8; 32],
            /// Whether it writes anything.
            speaks: bool,
        },
        /// The agent's own watch code ([`aterm_vz_agent::watch::run`], over the agent's
        /// Fake `System`, beating every `tick_ms`), on a thread, with the key it read from
        /// the plan's key disk.
        RealAgent {
            /// The hash its `Hello` names.
            agent_sha256: [u8; 32],
            /// Its tick, in milliseconds.
            tick_ms: u64,
        },
    }

    /// The key a guest's root reads: the first of `plan`'s disks that is a key disk, or
    /// zeros where none is.
    #[must_use]
    pub fn plan_key(plan: &Plan) -> aterm_vz_agent::key::Key {
        plan.disks
            .iter()
            .find_map(|d| aterm_vz_agent::key::read_disk(&d.path).ok().flatten())
            .unwrap_or([0; aterm_vz_agent::key::LEN])
    }

    /// The scripted agent's hello, with `key`.
    #[must_use]
    pub fn hello(
        agent_sha256: [u8; 32],
        key: aterm_vz_agent::key::Key,
    ) -> aterm_vz_agent::wire::Hello {
        aterm_vz_agent::wire::Hello {
            proto: aterm_vz_agent::wire::PROTO,
            key,
            agent_sha256,
            boot_id: BOOT_ID.into(),
            kernel: "6.8.0-142-generic".into(),
            uptime_ms: 1830,
        }
    }

    /// The boot id the scripted agent's hello names.
    pub const BOOT_ID: &str = "fake-boot-0001";

    impl Default for Script {
        fn default() -> Self {
            Self {
                limits: Ok(Limits {
                    max_cpus: 64,
                    max_memory: 128 << 30,
                    host_cpus: 18,
                    host_memory: 128 << 30,
                }),
                boot: Ok(()),
                on_boot: vec![FakeEvent::Started],
                hard_stop_completes: true,
                connect_refusal: None,
                share_refusal: None,
                guest_obeys: false,
            }
        }
    }

    /// Counts a connection object's life.
    #[derive(Debug)]
    struct Tracker(Arc<AtomicUsize>);

    impl Drop for Tracker {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// What the tests read back.
    #[derive(Clone, Debug, Default)]
    pub struct Seen {
        /// Every call, in order.
        pub calls: Arc<Mutex<Vec<Call>>>,
        /// The connection objects alive now.
        pub live_conns: Arc<AtomicUsize>,
        /// The guest's end of each connection made, in order.
        pub guest_ends: Arc<Mutex<Vec<UnixStream>>>,
        /// The sink `boot` was handed.
        pub events: Arc<Mutex<Option<Sink>>>,
        /// The scripted agent's end of its watch connection.
        pub watch_end: Arc<Mutex<Option<UnixStream>>>,
        /// Each forger's end of its dial, in order.
        pub forger_ends: Arc<Mutex<Vec<UnixStream>>>,
    }

    impl Seen {
        /// The calls so far.
        #[must_use]
        pub fn calls(&self) -> Vec<Call> {
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }

        /// The connection objects alive now.
        #[must_use]
        pub fn live(&self) -> usize {
            self.live_conns.load(Ordering::SeqCst)
        }

        /// Send `event` as the VM would, once booted.
        ///
        /// # Panics
        /// Before `boot`.
        pub fn send(&self, event: Event) {
            let events = self
                .events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            events.as_ref().expect("booted").send(event);
        }
    }

    /// The Fake VM.
    #[derive(Debug)]
    pub struct Fake {
        script: Script,
        seen: Seen,
    }

    impl Fake {
        /// A Fake answering `script`, and what the tests read back.
        #[must_use]
        pub fn new(script: Script) -> (Self, Seen) {
            let seen = Seen::default();
            (
                Self {
                    script,
                    seen: seen.clone(),
                },
                seen,
            )
        }

        fn call(&self, call: Call) {
            self.seen
                .calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(call);
        }
    }

    /// A connection pair: the helper's [`Conn`] and the guest's end, which the Fake keeps.
    ///
    /// # Panics
    /// A socket pair could not be made.
    #[must_use]
    pub fn conn_pair(port: u32, live: &Arc<AtomicUsize>) -> (Conn, UnixStream) {
        let (ours, guest) = UnixStream::pair().expect("a socket pair");
        let ours = OwnedFd::from(ours);
        let fd = ours.as_raw_fd();
        live.fetch_add(1, Ordering::SeqCst);
        let object: Box<dyn std::any::Any + Send> = Box::new((ours, Tracker(Arc::clone(live))));
        // SAFETY: `object` owns `ours`, the descriptor `fd` names, and closes it only when
        // it is dropped.
        (unsafe { Conn::new(fd, port, object) }, guest)
    }

    impl Machine for Fake {
        fn limits(&mut self) -> Result<Limits, String> {
            self.call(Call::Limits);
            self.script.limits.clone()
        }

        fn boot(&mut self, plan: &Plan, _limits: &Limits, events: Sink) -> Result<(), String> {
            self.call(Call::Boot(Box::new(plan.clone())));
            self.script.boot.clone()?;
            for e in &self.script.on_boot {
                events.send(match e {
                    FakeEvent::Started => Event::Started,
                    FakeEvent::StartFailed(why) => Event::StartFailed(why.clone()),
                    FakeEvent::Console(b) => Event::Console(b.clone()),
                    FakeEvent::Agent {
                        agent_sha256,
                        ready,
                    } => {
                        let (conn, mut guest) =
                            conn_pair(aterm_vz_agent::WATCH_PORT, &self.seen.live_conns);
                        let mut say = |frame| {
                            aterm_vz_agent::wire::write_frame(&mut guest, &frame)
                                .expect("the scripted agent writes into its socket buffer");
                        };
                        say(aterm_vz_agent::wire::Frame::Hello(hello(
                            *agent_sha256,
                            plan_key(plan),
                        )));
                        if *ready {
                            say(aterm_vz_agent::wire::Frame::Ready(
                                aterm_vz_agent::wire::Ready {
                                    state: "running".into(),
                                    failed_units: Vec::new(),
                                },
                            ));
                        }
                        *self
                            .seen
                            .watch_end
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(guest);
                        Event::Watch(conn)
                    }
                    FakeEvent::Forger {
                        agent_sha256,
                        speaks,
                    } => {
                        let (conn, mut guest) =
                            conn_pair(aterm_vz_agent::WATCH_PORT, &self.seen.live_conns);
                        if *speaks {
                            let mut forged = hello(*agent_sha256, [0x11; aterm_vz_agent::key::LEN]);
                            forged.boot_id = "forged".into();
                            for frame in [
                                aterm_vz_agent::wire::Frame::Hello(forged),
                                aterm_vz_agent::wire::Frame::Ready(aterm_vz_agent::wire::Ready {
                                    state: "running".into(),
                                    failed_units: Vec::new(),
                                }),
                                aterm_vz_agent::wire::Frame::Beat { uptime_ms: 1 },
                            ] {
                                aterm_vz_agent::wire::write_frame(&mut guest, &frame)
                                    .expect("the forger writes into its socket buffer");
                            }
                        }
                        self.seen
                            .forger_ends
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(guest);
                        Event::Watch(conn)
                    }
                    FakeEvent::RealAgent {
                        agent_sha256,
                        tick_ms,
                    } => {
                        let (conn, guest) =
                            conn_pair(aterm_vz_agent::WATCH_PORT, &self.seen.live_conns);
                        let said = hello(*agent_sha256, plan_key(plan));
                        let tick = std::time::Duration::from_millis(*tick_ms);
                        let system: Arc<dyn aterm_vz_agent::system::System> =
                            Arc::new(aterm_vz_agent::fake::Fake::new());
                        std::thread::Builder::new()
                            .name("fake-guest agent".into())
                            .spawn(move || aterm_vz_agent::watch::run(guest, &system, &said, tick))
                            .expect("the agent's thread starts");
                        Event::Watch(conn)
                    }
                });
            }
            *self
                .seen
                .events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(events);
            Ok(())
        }

        fn connect(&mut self, port: u32) -> Result<Conn, String> {
            self.call(Call::Connect(port));
            if let Some(why) = &self.script.connect_refusal {
                return Err(why.clone());
            }
            let (conn, guest) = conn_pair(port, &self.seen.live_conns);
            if self.script.guest_obeys
                && port == aterm_vz_agent::COMMAND_PORT
                && let Ok(mut reader) = guest.try_clone()
            {
                // The guest's agent: a `Poweroff` on this connection powers the guest off.
                let events = Arc::clone(&self.seen.events);
                std::thread::spawn(move || {
                    while let Ok(Some(frame)) = aterm_vz_agent::wire::read_frame(&mut reader) {
                        if frame == aterm_vz_agent::wire::Frame::Poweroff {
                            let sink = events
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .clone();
                            if let Some(sink) = sink {
                                sink.send(Event::GuestStopped);
                            }
                            return;
                        }
                    }
                });
            }
            self.seen
                .guest_ends
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(guest);
            Ok(conn)
        }

        fn share(&mut self, entries: &[ShareEntry]) -> Result<(), String> {
            self.call(Call::Share(entries.to_vec()));
            self.script.share_refusal.clone().map_or(Ok(()), Err)
        }

        fn request_stop(&mut self) -> Result<(), String> {
            self.call(Call::RequestStop);
            if self.script.guest_obeys {
                let events = self
                    .seen
                    .events
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(events) = events.as_ref() {
                    events.send(Event::GuestStopped);
                }
            }
            Ok(())
        }

        fn hard_stop(&mut self) -> Result<(), String> {
            self.call(Call::HardStop);
            if self.script.hard_stop_completes {
                let events = self
                    .seen
                    .events
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(events) = events.as_ref() {
                    events.send(Event::HardStopped(Ok(())));
                }
            }
            Ok(())
        }
    }
}
