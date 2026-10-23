// Copyright 2022, 2023 The AccessKit Authors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from accesskit_unix 0.22.0 (MIT OR Apache-2.0, used under
// Apache-2.0) src/context.rs and src/atspi/bus.rs, and rewritten by the aterm
// project in 2026 as one blocking thread over aterm-dbus: no executor, no
// zbus, and a lost bus is logged and survived instead of unwrapped.

//! AT-SPI publisher thread managing session and accessibility D-Bus connections.

use std::io::{self, ErrorKind, Read as _, Write as _};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use aterm_dbus::{Connection, Error as BusError, Kind, Message, address};

use super::adapter::{Adapter, AdapterState, Entry};
use super::semantics::{
    AppContext, Event, InterfaceSet, NodeIdentity, PlatformNode, PlatformRoot, SharedLock,
};
use super::serve::{self, Objects};
use super::{events, proxies};
use crate::model::{ActionHandler, ActivationHandler, DeactivationHandler};

/// Maximum duration a bus write waits for its daemon to read.
pub const WRITE: Duration = Duration::from_secs(5);

/// Initial backoff before retrying an accessibility bus join.
pub(crate) const REJOIN_FIRST: Duration = Duration::from_secs(1);

/// Maximum backoff between attempts to join the accessibility bus.
pub(crate) const REJOIN_MAX: Duration = Duration::from_secs(60);

/// Messages sent from windows to the publisher thread.
pub(crate) enum Msg {
    AddAdapter {
        id: usize,
        activation_handler: Box<dyn ActivationHandler + Send>,
        deactivation_handler: Box<dyn DeactivationHandler + Send>,
        state: Arc<Mutex<AdapterState>>,
    },
    RemoveAdapter {
        id: usize,
    },
    /// A change to a tree built while the publisher was on its `epoch`th
    /// accessibility bus.
    Tree {
        epoch: u64,
        change: Change,
    },
    /// The [`Publisher`] was dropped: end the thread.
    Shutdown,
}

/// A published tree's registrations and events.
pub(crate) enum Change {
    RegisterInterfaces {
        node: PlatformNode,
        interfaces: InterfaceSet,
    },
    UnregisterInterfaces {
        adapter: usize,
        node: NodeIdentity,
        interfaces: InterfaceSet,
    },
    Event {
        adapter: usize,
        event: Event,
    },
    CacheAdd {
        node: PlatformNode,
    },
    CacheRemove {
        adapter: usize,
        node: NodeIdentity,
    },
}

/// The windows' end of the channel to the publisher thread.
#[derive(Clone)]
pub(crate) struct Link {
    tx: Sender<Msg>,
    /// `None` when the OS gave no socket pair, and so no thread was started.
    wake: Option<Arc<UnixStream>>,
    app: Arc<SharedLock<AppContext>>,
}

impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Link").finish_non_exhaustive()
    }
}

impl Link {
    /// Queues `msg` and wakes the thread.
    pub(crate) fn send(&self, msg: Msg) {
        if self.tx.send(msg).is_ok()
            && let Some(wake) = &self.wake
        {
            // Non-blocking: a full socket already holds a wake.
            let _ = (&**wake).write(&[1]);
        }
    }

    /// The application root shared by all window trees.
    pub(crate) fn app(&self) -> &Arc<SharedLock<AppContext>> {
        &self.app
    }
}

/// How the publisher reaches its D-Bus connections.
pub trait Buses: Send + 'static {
    /// The session bus, where at-spi2-core's `org.a11y.Bus` lives.
    ///
    /// # Errors
    ///
    /// When it cannot be reached; the publisher then publishes nothing.
    fn session(&mut self) -> aterm_dbus::Result<Connection>;

    /// The bus at `address`, a D-Bus address list.
    ///
    /// # Errors
    ///
    /// When it cannot be reached.
    fn open(&mut self, address: &str) -> aterm_dbus::Result<Connection>;

    /// An accessibility bus address that overrides `org.a11y.Bus.GetAddress`.
    fn address_override(&self) -> Option<String> {
        None
    }

    /// How long a write to either bus may wait for its daemon to read.
    fn write_timeout(&self) -> Option<Duration> {
        Some(WRITE)
    }

    /// The wait before the first attempt to join the accessibility bus again.
    fn rejoin_first(&self) -> Duration {
        REJOIN_FIRST
    }
}

/// Desktop session and AT-SPI accessibility buses.
#[derive(Debug)]
pub struct SessionBuses {
    /// How long a join's handshake may wait: [`WRITE`] (a test's is short).
    join: Duration,
}

impl Default for SessionBuses {
    fn default() -> SessionBuses {
        SessionBuses { join: WRITE }
    }
}

#[cfg(test)]
impl SessionBuses {
    /// The desktop's buses, a join giving up after `join`.
    pub(crate) fn joining_within(join: Duration) -> SessionBuses {
        SessionBuses { join }
    }
}

impl Buses for SessionBuses {
    fn session(&mut self) -> aterm_dbus::Result<Connection> {
        Connection::session()
    }

    /// The handshake within [`WRITE`]: a frozen daemon's listener still
    /// takes the connection into its backlog, and would otherwise hold each
    /// attempt to join for aterm-dbus's `HANDSHAKE_TIMEOUT`.
    fn open(&mut self, list: &str) -> aterm_dbus::Result<Connection> {
        Connection::open_within(&address::parse(list)?, self.join)
    }

    fn address_override(&self) -> Option<String> {
        std::env::var("AT_SPI_BUS_ADDRESS")
            .ok()
            .filter(|a| !a.is_empty())
    }
}

/// A running AT-SPI publisher handle.
#[derive(Clone, Debug)]
pub struct Publisher {
    inner: Arc<Handle>,
}

#[derive(Debug)]
struct Handle {
    link: Link,
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.link.send(Msg::Shutdown);
    }
}

impl Publisher {
    /// Process-global publisher on the desktop buses, started on first use.
    pub fn global() -> &'static Publisher {
        static GLOBAL: OnceLock<Publisher> = OnceLock::new();
        GLOBAL.get_or_init(|| {
            let name = std::env::current_exe()
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
            Publisher::start(SessionBuses::default(), name)
        })
    }

    /// Starts a publisher on `buses` whose application root is named `app_name`.
    pub fn start(buses: impl Buses, app_name: Option<String>) -> Publisher {
        let (tx, rx) = mpsc::channel();
        let app = AppContext::new(app_name);
        let pair = UnixStream::pair().and_then(|(w, r)| {
            w.set_nonblocking(true)?;
            r.set_nonblocking(true)?;
            Ok((w, r))
        });
        let (wake, wake_rx) = match pair {
            Ok((wake, wake_rx)) => (Some(Arc::new(wake)), Some(wake_rx)),
            Err(e) => {
                aterm_log::warn!("accessibility: no wake socket ({e}); nothing is published");
                (None, None)
            }
        };
        let link = Link { tx, wake, app };
        if let Some(wake_rx) = wake_rx {
            let thread_link = link.clone();
            let spawned = std::thread::Builder::new()
                .name("aterm-atspi".into())
                .spawn(move || run(Box::new(buses), thread_link, rx, wake_rx));
            if let Err(e) = spawned {
                aterm_log::warn!("accessibility: no publisher thread ({e}); nothing is published");
            }
        }
        Publisher {
            inner: Arc::new(Handle { link }),
        }
    }

    /// A new window's adapter. Its handlers run on the publisher thread.
    pub fn adapter(
        &self,
        activation_handler: impl 'static + ActivationHandler + Send,
        action_handler: impl 'static + ActionHandler + Send,
        deactivation_handler: impl 'static + DeactivationHandler + Send,
    ) -> Adapter {
        Adapter::new(
            self.inner.link.clone(),
            activation_handler,
            action_handler,
            deactivation_handler,
        )
    }
}

/// The accessibility bus while attached, and what it serves.
struct A11yBus {
    conn: Connection,
    objects: Objects,
    /// Which attachment this is: the trees built for it carry the number.
    epoch: u64,
}

impl A11yBus {
    /// Applies a change to one of this attachment's trees; the signal it
    /// sends, if any.
    fn apply(&mut self, change: Change) -> Option<Message> {
        let name = self.objects.bus_name();
        match change {
            Change::RegisterInterfaces { node, interfaces } => {
                self.objects.register(node, interfaces);
                None
            }
            Change::UnregisterInterfaces {
                adapter,
                node,
                interfaces,
            } => {
                let id = super::wire::ObjectId::Node { adapter, node };
                self.objects.unregister(id, interfaces);
                None
            }
            Change::Event { adapter, event } => match event {
                Event::Object { target, event } => {
                    Some(events::object_event(name, adapter, target, event))
                }
                Event::Window {
                    target,
                    name: window,
                    event,
                } => Some(events::window_event(adapter, target, window, event)),
                Event::Cache(_) => None,
            },
            Change::CacheAdd { node } => {
                let item = node.cache_node().ok()?;
                Some(events::cache_add(name, item))
            }
            Change::CacheRemove { adapter, node } => {
                Some(events::cache_remove(name, adapter, node))
            }
        }
    }
}

struct Worker {
    buses: Box<dyn Buses>,
    link: Link,
    rx: Receiver<Msg>,
    wake: UnixStream,
    session: Connection,
    /// The session bus serves nothing, but answers what it is asked.
    session_objects: Objects,
    bus: Option<A11yBus>,
    /// Attachments so far; the current one's number while attached.
    epochs: u64,
    adapters: Vec<Entry>,
    /// The next attempt to join, while assistive technologies want the tree
    /// and the last attempt failed.
    rejoin: Option<Rejoin>,
}

/// Whether a bus error means the connection is gone.
fn lost(e: &BusError) -> bool {
    matches!(e, BusError::Closed | BusError::Io(_))
}

/// Whether a failed write to the accessibility bus means leaving it: it is
/// gone, or its daemon took nothing within [`Buses::write_timeout`].
fn left(e: &BusError) -> bool {
    lost(e) || matches!(e, BusError::Timeout)
}

/// Next scheduled attempt to join the accessibility bus.
#[derive(Clone, Copy, Debug)]
pub(super) struct Rejoin {
    pub(super) at: Instant,
    pub(super) after: Duration,
}

impl Rejoin {
    /// Compute the next rejoin backoff after `last`.
    pub(super) fn next(last: Option<Rejoin>, now: Instant, first: Duration) -> Option<Rejoin> {
        let after = last.map_or(first, |r| r.after.saturating_mul(2).min(REJOIN_MAX));
        now.checked_add(after).map(|at| Rejoin { at, after })
    }
}

fn run(mut buses: Box<dyn Buses>, link: Link, rx: Receiver<Msg>, wake: UnixStream) {
    let session = match buses.session() {
        Ok(mut session) => {
            session.set_send_timeout(buses.write_timeout());
            session
        }
        Err(e) => {
            aterm_log::warn!(
                "accessibility: no session bus ({e}); screen readers cannot reach this process"
            );
            return;
        }
    };
    let session_objects = Objects::new(session.unique_name(), PlatformRoot::new(link.app()));
    let mut worker = Worker {
        buses,
        link,
        rx,
        wake,
        session,
        session_objects,
        bus: None,
        epochs: 0,
        adapters: Vec::new(),
        rejoin: None,
    };
    for rule in [proxies::STATUS_RULE, proxies::OWNER_RULE] {
        if let Err(e) = worker.session.add_match(rule, proxies::CALL) {
            aterm_log::warn!("accessibility: AddMatch {rule}: {e}");
        }
    }
    if worker.refresh_status().is_ok() {
        worker.serve();
    }
    worker.detach("the publisher stopped");
}

impl Worker {
    /// The loop: messages from windows, then whatever the buses have.
    fn serve(&mut self) {
        loop {
            if !self.drain_channel() {
                return;
            }
            let buffered = self.session.has_buffered()
                || self.bus.as_ref().is_some_and(|b| b.conn.has_buffered());
            let mut fds = vec![self.wake.as_raw_fd(), self.session.as_raw_fd()];
            fds.extend(self.bus.as_ref().map(|b| b.conn.as_raw_fd()));
            let timeout = if buffered {
                Some(Duration::ZERO)
            } else {
                self.rejoin
                    .map(|r| r.at.saturating_duration_since(Instant::now()))
            };
            let ready = match wait(&fds, timeout) {
                Ok(ready) => ready,
                Err(e) => {
                    aterm_log::warn!("accessibility: poll failed ({e}); publishing stops");
                    return;
                }
            };
            if ready.first().copied().unwrap_or(false) {
                let mut sink = [0u8; 64];
                while matches!((&self.wake).read(&mut sink), Ok(n) if n > 0) {}
            }
            if (ready.get(1).copied().unwrap_or(false) || self.session.has_buffered())
                && self.pump_session().is_err()
            {
                return;
            }
            if ready.get(2).copied().unwrap_or(false)
                || self.bus.as_ref().is_some_and(|b| b.conn.has_buffered())
            {
                self.pump_bus();
            }
            if self.rejoin.is_some_and(|r| r.at <= Instant::now()) && self.refresh_status().is_err()
            {
                return;
            }
        }
    }

    /// Asks `IsEnabled` and attaches or detaches to match. `Err` when the
    /// session bus is gone.
    fn refresh_status(&mut self) -> Result<(), ()> {
        let enabled = match proxies::is_enabled(&mut self.session) {
            Ok(enabled) => enabled,
            Err(e) if lost(&e) => {
                aterm_log::warn!(
                    "accessibility: the session bus closed ({e}); screen readers cannot reach \
                     this process until it restarts"
                );
                self.detach("the session bus closed");
                return Err(());
            }
            Err(e) if self.rejoin.is_some() => {
                aterm_log::debug!(
                    "accessibility: org.a11y.Status.IsEnabled: {e}; trying again later"
                );
                self.rejoin = Rejoin::next(self.rejoin, Instant::now(), self.buses.rejoin_first());
                return Ok(());
            }
            Err(e) => {
                aterm_log::debug!("accessibility: org.a11y.Status.IsEnabled: {e}");
                false
            }
        };
        match (enabled, self.bus.is_some()) {
            (true, false) => self.attach(),
            (false, true) => self.detach("assistive technologies were turned off"),
            _ => {}
        }
        if !enabled || self.bus.is_some() {
            self.rejoin = None;
        }
        Ok(())
    }

    /// Joins the accessibility bus, lists the application with the
    /// registry, and asks every window for its tree.
    fn attach(&mut self) {
        let address = match self.buses.address_override() {
            Some(address) => Ok(address),
            None => proxies::bus_address(&mut self.session),
        };
        let write_timeout = self.buses.write_timeout();
        let joined = address.and_then(|a| {
            let mut conn = self.buses.open(&a)?;
            conn.set_send_timeout(write_timeout);
            proxies::embed(&mut conn)?;
            Ok(conn)
        });
        let conn = match joined {
            Ok(conn) => conn,
            Err(e) => {
                let rejoin = Rejoin::next(self.rejoin, Instant::now(), self.buses.rejoin_first());
                let again = rejoin.map_or_else(
                    || "when at-spi2-core's status changes".to_string(),
                    |r| {
                        format!(
                            "in {} s and when at-spi2-core's status changes",
                            r.after.as_secs()
                        )
                    },
                );
                if self.rejoin.is_none() {
                    aterm_log::warn!(
                        "accessibility: cannot join the AT-SPI bus ({e}); trying again {again}"
                    );
                } else {
                    aterm_log::debug!(
                        "accessibility: still cannot join the AT-SPI bus ({e}); trying again \
                         {again}"
                    );
                }
                self.rejoin = rejoin;
                return;
            }
        };
        let mut objects = Objects::new(conn.unique_name(), PlatformRoot::new(self.link.app()));
        objects.publish_root();
        aterm_log::info!(
            "accessibility: publishing on the AT-SPI bus as {}",
            conn.unique_name()
        );
        self.epochs = self.epochs.wrapping_add(1);
        let epoch = self.epochs;
        self.bus = Some(A11yBus {
            conn,
            objects,
            epoch,
        });
        for entry in &mut self.adapters {
            entry.activate(&self.link, epoch);
        }
    }

    /// Leaves the accessibility bus, if on it, and deactivates every window.
    fn detach(&mut self, why: &str) {
        if self.bus.take().is_some() {
            aterm_log::info!("accessibility: left the AT-SPI bus: {why}");
        }
        for entry in &mut self.adapters {
            entry.deactivate();
        }
    }

    /// The accessibility bus failed, or took no write in time: say so, drop
    /// the trees, and see whether at-spi2-core has a bus to rejoin.
    fn bus_lost(&mut self, e: &BusError) {
        let why = if matches!(e, BusError::Timeout) {
            let waited = self.buses.write_timeout().unwrap_or(WRITE).as_secs_f64();
            aterm_log::warn!(
                "accessibility: the AT-SPI bus stopped reading (a write waited {waited} s); \
                 screen readers lose this process until it rejoins"
            );
            "the AT-SPI bus stopped reading"
        } else {
            aterm_log::warn!(
                "accessibility: the AT-SPI bus failed ({e}); screen readers lose this process \
                 until it rejoins"
            );
            "the AT-SPI bus failed"
        };
        self.detach(why);
        let _ = self.refresh_status();
    }

    /// Everything the windows queued. `false` once the publisher is dropped.
    fn drain_channel(&mut self) -> bool {
        loop {
            let msg = match self.rx.try_recv() {
                Ok(msg) => msg,
                Err(TryRecvError::Empty) => return true,
                Err(TryRecvError::Disconnected) => return false,
            };
            if !self.handle(msg) {
                return false;
            }
        }
    }

    fn handle(&mut self, msg: Msg) -> bool {
        let signal = match msg {
            Msg::Shutdown => return false,
            Msg::AddAdapter {
                id,
                activation_handler,
                deactivation_handler,
                state,
            } => {
                let mut entry = Entry {
                    id,
                    activation_handler,
                    deactivation_handler,
                    state,
                };
                if let Some(bus) = &self.bus {
                    entry.activate(&self.link, bus.epoch);
                }
                self.adapters.push(entry);
                None
            }
            Msg::RemoveAdapter { id } => {
                self.adapters.retain(|entry| entry.id != id);
                if let Some(bus) = &mut self.bus {
                    bus.objects.forget_adapter(id);
                }
                None
            }
            Msg::Tree { epoch, change } => match &mut self.bus {
                Some(bus) if bus.epoch == epoch => bus.apply(change),
                _ => None,
            },
        };
        if let Some(signal) = signal
            && let Some(bus) = &mut self.bus
            && let Err(e) = bus.conn.send(signal)
        {
            if left(&e) {
                self.bus_lost(&e);
            } else {
                aterm_log::debug!("accessibility: an event was not sent: {e}");
            }
        }
        true
    }

    /// Reads the session bus: answers calls, and re-reads the status when it
    /// may have changed.
    fn pump_session(&mut self) -> Result<(), ()> {
        let mut recheck = false;
        loop {
            match self.session.recv(Some(std::time::Duration::ZERO)) {
                Ok(Some(msg)) => {
                    recheck |= proxies::status_may_have_changed(&msg);
                    if msg.kind == Kind::MethodCall {
                        let served = serve::serve(&self.session_objects, &msg);
                        let _ = answer(&mut self.session, &msg, served);
                    }
                }
                Ok(None) => break,
                Err(e) if lost(&e) => {
                    aterm_log::warn!(
                        "accessibility: the session bus closed ({e}); screen readers cannot \
                         reach this process until it restarts"
                    );
                    self.detach("the session bus closed");
                    return Err(());
                }
                Err(e) => aterm_log::debug!("accessibility: session bus: {e}"),
            }
        }
        if recheck {
            self.refresh_status()?;
        }
        Ok(())
    }

    /// Reads the accessibility bus and answers its calls.
    fn pump_bus(&mut self) {
        loop {
            let Some(bus) = &mut self.bus else {
                return;
            };
            let result = match bus.conn.recv(Some(std::time::Duration::ZERO)) {
                Ok(Some(msg)) if msg.kind == Kind::MethodCall => {
                    let served = serve::serve(&bus.objects, &msg);
                    answer(&mut bus.conn, &msg, served)
                }
                Ok(Some(_)) => Ok(()),
                Ok(None) => return,
                Err(e) => Err(e),
            };
            match result {
                Ok(()) => {}
                Err(e) if left(&e) => {
                    self.bus_lost(&e);
                    return;
                }
                Err(e) => aterm_log::debug!("accessibility: AT-SPI bus: {e}"),
            }
        }
    }
}

/// Sends what a call produced: its signals, then its reply or error.
fn answer(conn: &mut Connection, call: &Message, served: serve::Served) -> aterm_dbus::Result<()> {
    for signal in served.signals {
        conn.send(signal)?;
    }
    match served.answer {
        Ok(body) => conn.reply(call, body),
        Err(fault) => conn.reply_error(call, fault.name, &fault.text),
    }
}

/// Waits until one of `fds` is readable or hung up, or `timeout` has passed
/// (`None` waits for a descriptor; zero only looks); says which are.
#[cfg_attr(trust_verify, trust::skip)]
#[expect(
    unsafe_code,
    reason = "the crate's one FFI call: poll(2) over an array it owns"
)]
fn wait(fds: &[RawFd], timeout: Option<Duration>) -> io::Result<Vec<bool>> {
    let mut polls: Vec<libc::pollfd> = fds
        .iter()
        .map(|&fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    let count = libc::nfds_t::try_from(polls.len()).unwrap_or(libc::nfds_t::MAX);
    let millis = timeout.map_or(-1, |t| {
        let ms = t.as_micros().div_ceil(1000);
        libc::c_int::try_from(ms).unwrap_or(libc::c_int::MAX)
    });
    loop {
        // SAFETY: `polls` is an initialized array of `count` pollfd records alive for the call.
        let n = unsafe { libc::poll(polls.as_mut_ptr(), count, millis) };
        if n >= 0 {
            return Ok(polls.iter().map(|p| p.revents != 0).collect());
        }
        let e = io::Error::last_os_error();
        if e.kind() != ErrorKind::Interrupted {
            return Err(e);
        }
    }
}
