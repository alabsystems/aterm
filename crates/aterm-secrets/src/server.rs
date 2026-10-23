// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keyless HTTP vault server ([`serve`], [`Running::stop`]) enforcing connection and body caps
//! ([`MAX_CONNECTIONS`], [`MAX_PEER_CONNECTIONS`], [`MAX_PUTS`], [`MAX_PEER_PUTS`],
//! [`crate::envelope::MAX_ENVELOPE`]), signer pins and handoff chains ([`crate::ALAB_SIGNER`],
//! [`crate::ALAB_RETIRED`], [`crate::envelope::Envelope::check_pin`],
//! [`crate::envelope::Envelope::check_extends`]), and join queues ([`JOIN_CHANGE_GAP`],
//! [`crate::transport::parse_join_line`]).

use crate::envelope::{self, MAX_ENVELOPE};
use crate::transport::{JoinRequest, parse_join_line};
use crate::{Error, fsx};
use std::collections::{BTreeMap, HashMap};
use std::io::{self, Read, Write};
use std::net::{
    IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How to serve.
#[derive(Debug, Clone)]
pub struct Options {
    /// Where to listen.
    pub listen: SocketAddr,
    /// The store directory (made 0700).
    pub store: PathBuf,
    /// Optional `<name>.local` mDNS registration on the LAN's IPv4 address (macOS only).
    pub advertise: Option<String>,
    /// Pins for names the store holds nothing for: name → signing key.
    pub pins: Vec<(String, [u8; 32])>,
    /// How long a long-poll is held.
    pub hold: Duration,
    /// macOS: hold `caffeinate -i -w <pid>` so the Mac does not idle-sleep under the server.
    pub keep_awake: bool,
}

/// Default hold duration for a long-poll.
pub const DEFAULT_HOLD: Duration = Duration::from_secs(50);
/// The most connections served at once.
pub const MAX_CONNECTIONS: usize = 1024;
/// The most connections served at once from one address ([`MAX_CONNECTIONS`]).
pub const MAX_PEER_CONNECTIONS: usize = 16;
/// The most PUT bodies read at once (each up to [`MAX_ENVELOPE`]).
pub const MAX_PUTS: usize = 4;
/// The most PUT bodies read at once from one address of another machine.
pub const MAX_PEER_PUTS: usize = 1;
/// A connection thread's stack.
const CONNECTION_STACK: usize = 256 * 1024;
/// The largest request head, its blank line included.
pub const MAX_HEAD: usize = 16 * 1024;
/// How long a client has to send its whole head.
pub const HEAD_DEADLINE: Duration = Duration::from_secs(10);
/// How long a client has to send a body, and the longest it may stall within it.
const BODY_DEADLINE: Duration = Duration::from_secs(120);
const BODY_STALL: Duration = Duration::from_secs(10);
/// The longest one write of an answer may block, and the longest the whole answer may take.
const WRITE_STALL: Duration = Duration::from_secs(60);
const ANSWER_DEADLINE: Duration = Duration::from_secs(300);
/// The most an answer is handed to the socket in one write.
const WRITE_CHUNK: usize = 64 * 1024;
/// How long the accept loop gives a 503 over a cap (a fresh socket takes one line at once).
const REFUSAL_DEADLINE: Duration = Duration::from_secs(1);
/// How many answered refusals may wait to be closed gently.
const REFUSALS_QUEUED: usize = 64;
/// How often a held long-poll looks whether its client went away.
const LIVENESS_EVERY: Duration = Duration::from_secs(5);
/// The largest join request body.
pub const MAX_JOIN_BODY: usize = 1024;
/// The most queued join requests per vault.
pub const MAX_JOINS: usize = 256;
/// The most queued join requests from one address per vault.
pub const MAX_PEER_JOINS: usize = 4;
/// The largest `.joins` file read ([`MAX_JOINS`]).
const MAX_JOINS_FILE: usize = MAX_JOINS * (MAX_JOIN_BODY + 128);
/// How long a join request stays queued.
pub const JOIN_TTL_SECS: u64 = 7 * 24 * 60 * 60;
/// How long a queued join request stands before its key may change it.
pub const JOIN_CHANGE_GAP: Duration = Duration::from_secs(10);
/// How often the log notes changes to one queued join request.
const JOIN_NOTE_EVERY_SECS: u64 = 60 * 60;
/// Minimum interval before an unchanged join re-post refreshes its queue timestamp ([`JOIN_TTL_SECS`]).
const JOIN_REFRESH_EVERY_SECS: u64 = 60 * 60;
/// The most vaults one server holds.
pub const MAX_VAULTS: usize = 64;
/// The mDNS service type advertised.
pub const SERVICE_TYPE: &str = "_alab-vault._tcp";
/// macOS's mDNS command.
pub const DNS_SD: &str = "/usr/bin/dns-sd";
/// macOS's sleep assertion command.
pub const CAFFEINATE: &str = "/usr/bin/caffeinate";
/// How often the advertised address is checked.
const ADVERTISE_EVERY: Duration = Duration::from_secs(30);
/// What a closing connection drains so its answer is not lost to a reset.
const LINGER_BYTES: usize = 1 << 20;
const LINGER_DEADLINE: Duration = Duration::from_secs(2);
/// How often [`serve`] looks whether its program was replaced on disk.
const PROGRAM_EVERY: Duration = Duration::from_secs(5);
/// How long a server stopping for a new program waits for the answers in flight.
const DRAIN_DEADLINE: Duration = Duration::from_secs(10);

/// Serve until the process ends, or until this program is replaced on disk (then `Ok`).
pub fn serve(options: Options) -> crate::Result<()> {
    if let Some(name) = &options.advertise
        && !envelope::valid_name(name)
    {
        return Err(Error::new(
            "--advertise takes a host label: lowercase letters, digits and -",
        ));
    }
    let signalled = Arc::new(AtomicBool::new(false));
    {
        let signalled = Arc::clone(&signalled);
        let main = std::thread::current();
        aterm_uds::process::take_stop_signals("vault-stop", move |_| {
            signalled.store(true, Ordering::SeqCst);
            main.unpark();
        });
    }
    let listener = TcpListener::bind(options.listen)
        .map_err(|e| Error::new(format!("listening on {}: {e}", options.listen)))?;
    let running = start(listener, &options)?;
    note(&format!(
        "serving the vaults in {} on {}",
        options.store.display(),
        running.addr
    ));
    let unheld = unheld_note(&running.shared.lock_server_state(), &options.pins);
    if let Some(line) = unheld {
        note(&line);
    }
    let children = Children::default();
    if options.keep_awake {
        children.keep_awake();
    }
    if let Some(name) = &options.advertise {
        children.advertise(name, running.addr.port());
    }
    let result = supervise(running, PROGRAM_EVERY, &program_stamp, &signalled);
    drop(children);
    result
}

/// An executable as it is on disk.
type Stamp = (u64, u64, u64, i64);

/// This process's executable as it is now (client.rs's subscriber watches the same).
fn program_stamp() -> Option<Stamp> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(std::env::current_exe().ok()?).ok()?;
    Some((meta.dev(), meta.ino(), meta.size(), meta.mtime()))
}

/// Run until the accept loop ends ([`Running::stop`], [`DRAIN_DEADLINE`]).
fn supervise(
    running: Running,
    every: Duration,
    stamp: &dyn Fn() -> Option<Stamp>,
    signalled: &AtomicBool,
) -> crate::Result<()> {
    let program = stamp();
    loop {
        if running.accept.as_ref().is_none_or(JoinHandle::is_finished) {
            return running.wait();
        }
        let stopping = if signalled.load(Ordering::SeqCst) {
            Some("stopped by a signal")
        } else if program.is_some() && stamp() != program {
            Some("this program was replaced on disk; exiting so the service starts the new one")
        } else {
            None
        };
        if let Some(why) = stopping {
            note(why);
            let shared = Arc::clone(&running.shared);
            running.stop();
            let started = Instant::now();
            while shared.active.load(Ordering::SeqCst) > 0 && started.elapsed() < DRAIN_DEADLINE {
                std::thread::sleep(Duration::from_millis(20));
            }
            return Ok(());
        }
        std::thread::park_timeout(every);
    }
}

/// A running server on a listener already bound (tests bind `127.0.0.1:0`) ([`Running::stop`]).
pub struct Running {
    /// Where it listens.
    pub addr: SocketAddr,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    shared: Arc<Shared>,
    accept: Option<JoinHandle<io::Result<()>>>,
}

impl Running {
    /// Stop accepting and return once the accept loop has ended.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.shared.stopping.store(true, Ordering::SeqCst);
        {
            let state = self.shared.lock_server_state();
            for slot in state.vaults.values() {
                slot.wake.notify_all();
            }
        }
        let wake = wake_addr(self.addr);
        if TcpStream::connect_timeout(&wake, Duration::from_secs(5)).is_ok()
            && let Some(accept) = self.accept.take()
        {
            let _ = accept.join();
        }
    }

    /// Wait for the accept loop to end (it ends only on a listener error).
    fn wait(mut self) -> crate::Result<()> {
        match self.accept.take().map(JoinHandle::join) {
            Some(Ok(Ok(()))) | None => Ok(()),
            Some(Ok(Err(e))) => Err(Error::new(format!("accepting connections: {e}"))),
            Some(Err(_)) => Err(Error::new("the accept loop panicked")),
        }
    }

    #[cfg(test)]
    fn waiting(&self) -> usize {
        self.shared.waiting.load(Ordering::SeqCst)
    }
}

/// The address that reaches a listener bound to `addr` from this machine.
fn wake_addr(addr: SocketAddr) -> SocketAddr {
    match addr.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => {
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), addr.port())
        }
        IpAddr::V6(ip) if ip.is_unspecified() => {
            SocketAddr::new(Ipv6Addr::LOCALHOST.into(), addr.port())
        }
        _ => addr,
    }
}

/// Start serving on `listener` in background threads (no mDNS, no caffeinate).
pub fn start(listener: std::net::TcpListener, options: &Options) -> crate::Result<Running> {
    launch(listener, options, Tuning::default())
}

/// [`start`] with the join queue's change gap `gap` in place of [`JOIN_CHANGE_GAP`].
#[cfg(any(test, debug_assertions))]
pub fn start_with_join_change_gap(
    listener: std::net::TcpListener,
    options: &Options,
    gap: Duration,
) -> crate::Result<Running> {
    launch(
        listener,
        options,
        Tuning {
            join_change_gap: gap,
            ..Tuning::default()
        },
    )
}

/// [`start`] as a server on another Mac than its clients' ([`refuse_unheld`]).
#[cfg(test)]
pub(crate) fn start_apart(
    listener: std::net::TcpListener,
    options: &Options,
) -> crate::Result<Running> {
    launch(
        listener,
        options,
        Tuning {
            is_local: |_, _| false,
            ..Tuning::default()
        },
    )
}

/// Server deadlines, clock, and local-connection predicate ([`this_machine`]).
#[derive(Clone, Copy)]
struct Tuning {
    head_deadline: Duration,
    body_deadline: Duration,
    body_stall: Duration,
    write_stall: Duration,
    answer_deadline: Duration,
    liveness_every: Duration,
    join_change_gap: Duration,
    /// How long a closing connection drains what its client still sends ([`linger`]).
    linger: Duration,
    /// The wall clock the join queue is kept by, in unix seconds ([`now`]).
    now: fn() -> u64,
    /// Whether a connection from a peer address, arriving on one of this machine's addresses
    /// (`None`: unknown), comes from this machine.
    is_local: fn(IpAddr, Option<IpAddr>) -> bool,
    /// The keys no PUT is taken signed by, and the key a held signer among them gives way to.
    retired: fn() -> envelope::Retired,
}

impl Default for Tuning {
    fn default() -> Tuning {
        Tuning {
            head_deadline: HEAD_DEADLINE,
            body_deadline: BODY_DEADLINE,
            body_stall: BODY_STALL,
            write_stall: WRITE_STALL,
            answer_deadline: ANSWER_DEADLINE,
            liveness_every: LIVENESS_EVERY,
            join_change_gap: JOIN_CHANGE_GAP,
            linger: LINGER_DEADLINE,
            now,
            is_local: this_machine,
            retired: envelope::Retired::compiled,
        }
    }
}

fn launch(listener: TcpListener, options: &Options, tuning: Tuning) -> crate::Result<Running> {
    for (name, _) in &options.pins {
        if !envelope::valid_name(name) {
            return Err(Error::new(
                "a pin names a vault that is not a vault name ([a-z0-9][a-z0-9-]{0,62})",
            ));
        }
    }
    fsx::ensure_private_dir(&options.store)?;
    let state = load(&options.store, (tuning.now)())?;
    let addr = listener.local_addr()?;
    let shared = Arc::new(Shared {
        store: options.store.clone(),
        pins: options.pins.clone(),
        retired: (tuning.retired)(),
        hold: options.hold,
        tuning,
        server_state: Mutex::new(state),
        stopping: AtomicBool::new(false),
        active: AtomicUsize::new(0),
        peers: Mutex::new(HashMap::new()),
        puts: Mutex::new(Puts::default()),
        join_writes: Mutex::new(()),
        #[cfg(test)]
        waiting: AtomicUsize::new(0),
        #[cfg(test)]
        served: AtomicUsize::new(0),
    });
    let (refusals, refused) = sync_channel::<TcpStream>(REFUSALS_QUEUED);
    std::thread::Builder::new()
        .name("vault-refuse".to_owned())
        .stack_size(CONNECTION_STACK)
        .spawn(move || {
            for mut stream in refused {
                linger(&mut stream, tuning.linger);
            }
        })?;
    let stop = Arc::new(AtomicBool::new(false));
    let accept = {
        let shared = Arc::clone(&shared);
        let stop = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("vault-accept".to_owned())
            .spawn(move || accept_loop(&listener, &shared, &stop, &refusals))?
    };
    Ok(Running {
        addr,
        stop,
        shared,
        accept: Some(accept),
    })
}

/// Whether a connection from `peer` comes from this machine.
fn this_machine(peer: IpAddr, arrived_on: Option<IpAddr>) -> bool {
    let peer = peer.to_canonical();
    peer.is_loopback()
        || arrived_on.is_some_and(|local| local.to_canonical() == peer)
        || primary_ipv4().is_some_and(|ip| IpAddr::V4(ip) == peer)
        || primary_ipv6().is_some_and(|ip| IpAddr::V6(ip) == peer)
}

/// What the server holds for one vault.
struct Held {
    seq: u64,
    signer: [u8; 32],
    /// The held envelope's handoff chain ([`envelope::Retired::gives_way`]).
    handoff: Vec<([u8; 32], [u8; 64])>,
    bytes: Arc<Vec<u8>>,
}

/// One vault and the condition its long-polls wait on.
struct Slot {
    held: Held,
    wake: Arc<Condvar>,
}

/// A queued join request and the address it came from (`None`: queued before addresses were kept).
#[derive(Clone)]
struct Queued {
    request: JoinRequest,
    peer: Option<IpAddr>,
    /// The vault's seq when the request was last asked (queued, changed or asked again).
    asked: u64,
    /// When its address last queued or changed it.
    changed: u64,
    /// When the log last noted it (unix secs; 0: not since the server started).
    noted: u64,
}

#[derive(Default)]
struct State {
    vaults: BTreeMap<String, Slot>,
    joins: BTreeMap<String, Vec<Queued>>,
}

struct Shared {
    store: PathBuf,
    pins: Vec<(String, [u8; 32])>,
    /// The keys the ALab vault rotated away from ([`crate::ALAB_RETIRED`]).
    retired: envelope::Retired,
    hold: Duration,
    tuning: Tuning,
    server_state: Mutex<State>,
    stopping: AtomicBool,
    active: AtomicUsize,
    /// Connections open now, by the address they came from.
    peers: Mutex<HashMap<IpAddr, usize>>,
    /// PUT bodies being read now.
    puts: Mutex<Puts>,
    /// Held while a join request is decided and its store written, taken before `server_state`.
    join_writes: Mutex<()>,
    #[cfg(test)]
    waiting: AtomicUsize,
    /// Connections admitted since the start.
    #[cfg(test)]
    served: AtomicUsize,
}

impl Shared {
    fn lock_server_state(&self) -> MutexGuard<'_, State> {
        self.server_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_peers(&self) -> MutexGuard<'_, HashMap<IpAddr, usize>> {
        self.peers.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_puts(&self) -> MutexGuard<'_, Puts> {
        self.puts.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn join_writes(&self) -> MutexGuard<'_, ()> {
        self.join_writes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn vault_path(&self, name: &str) -> PathBuf {
        self.store.join(format!("{name}.vault"))
    }

    fn joins_path(&self, name: &str) -> PathBuf {
        self.store.join(format!("{name}.joins"))
    }
}

fn now() -> u64 {
    aterm_time::unix_secs().unwrap_or(0)
}

/// A line for the server's log (stderr).
fn note(line: &str) {
    eprintln!("aterm secrets serve: {line}");
}

/// What [`serve`] logs at start when its store holds no vault yet ([`refuse_unheld`]).
fn unheld_note(state: &State, pins: &[(String, [u8; 32])]) -> Option<String> {
    if !state.vaults.is_empty() {
        return None;
    }
    let from = if pins.is_empty() {
        "only from this Mac, since no --pin names a vault".to_string()
    } else {
        let names: Vec<&str> = pins.iter().map(|(name, _)| name.as_str()).collect();
        format!(
            "from this Mac, or from another for a vault this server pins ({})",
            names.join(", ")
        )
    };
    Some(format!(
        "the store holds no vault yet: a Mac in the vault puts its copy here (its subscriber does, and any aterm secrets admin command there), and until then a machine that asks to join finds none; a first version is taken {from}"
    ))
}

/// Read the store: every `<name>.vault` that parses and verifies, and every `<name>.joins` line
/// that parses and has not expired.
fn load(store: &Path, now: u64) -> crate::Result<State> {
    let mut state = State::default();
    let entries =
        std::fs::read_dir(store).map_err(|e| Error::new(format!("{}: {e}", store.display())))?;
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .collect();
    names.sort();
    for file in names {
        let path = store.join(&file);
        if let Some(name) = file
            .strip_suffix(".vault")
            .filter(|n| envelope::valid_name(n))
        {
            if state.vaults.len() == MAX_VAULTS {
                note(&format!(
                    "{file}: skipped; the server holds at most {MAX_VAULTS} vaults"
                ));
                continue;
            }
            match load_vault(&path, name) {
                Ok(held) => {
                    state.vaults.insert(
                        name.to_owned(),
                        Slot {
                            held,
                            wake: Arc::new(Condvar::new()),
                        },
                    );
                }
                Err(e) => note(&format!("{file}: skipped: {e}")),
            }
        } else if let Some(name) = file
            .strip_suffix(".joins")
            .filter(|n| envelope::valid_name(n))
        {
            match fsx::read_owned(&path, MAX_JOINS_FILE) {
                Ok(Some(bytes)) => {
                    let mut queue = parse_stored(&String::from_utf8_lossy(&bytes));
                    expire(&mut queue, now);
                    queue.truncate(MAX_JOINS);
                    state.joins.insert(name.to_owned(), queue);
                }
                Ok(None) => {}
                Err(e) => note(&format!("{file}: skipped: {e}")),
            }
        }
    }
    Ok(state)
}

fn load_vault(path: &Path, name: &str) -> crate::Result<Held> {
    let bytes = fsx::read_owned(path, MAX_ENVELOPE)?.ok_or_else(|| Error::new("vanished"))?;
    let envelope = envelope::parse(&bytes)?;
    if envelope.name != name {
        return Err(Error::new(format!(
            "holds the vault `{}`, not `{name}`",
            envelope.name
        )));
    }
    envelope.verify_signature()?;
    Ok(Held {
        seq: envelope.seq,
        signer: envelope.signer,
        handoff: envelope.handoff,
        bytes: Arc::new(bytes),
    })
}

/// Drop requests queued more than [`JOIN_TTL_SECS`] from now either way.
fn expire(queue: &mut Vec<Queued>, now: u64) {
    queue.retain(|queued| {
        queued
            .request
            .time
            .is_some_and(|time| time.abs_diff(now) <= JOIN_TTL_SECS)
    });
}

/// The listing: each request's listed line, and nothing of where it came from.
fn join_lines(queue: &[Queued]) -> String {
    let mut text = String::new();
    for queued in queue {
        text.push_str(&queued.request.listed());
        text.push('\n');
    }
    text
}

/// The store's form of a queue ([`Queued::asked`]).
fn stored_lines(queue: &[Queued]) -> String {
    let mut text = String::new();
    for queued in queue {
        if let Some(peer) = queued.peer {
            text.push_str(&format!("peer {peer} {}\n", queued.asked));
        }
        text.push_str(&queued.request.listed());
        text.push('\n');
    }
    text
}

/// Read [`stored_lines`].
fn parse_stored(text: &str) -> Vec<Queued> {
    let mut queue = Vec::new();
    let mut peer = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("peer ") {
            let (address, asked) = rest.split_once(' ').unwrap_or((rest, "0"));
            peer = address
                .parse::<IpAddr>()
                .ok()
                .map(|ip| (ip, asked.parse::<u64>().unwrap_or(0)));
            continue;
        }
        let (from, asked) = peer.take().unzip();
        if let Ok(request) = parse_join_line(line)
            && request.time.is_some()
        {
            queue.push(Queued {
                request,
                peer: from,
                asked: asked.unwrap_or(0),
                changed: 0,
                noted: 0,
            });
        }
    }
    queue
}

fn accept_loop(
    listener: &TcpListener,
    shared: &Arc<Shared>,
    stop: &AtomicBool,
    refusals: &SyncSender<TcpStream>,
) -> io::Result<()> {
    loop {
        let accepted = listener.accept();
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        match accepted {
            Ok((stream, peer)) => dispatch(shared, stream, peer.ip(), refusals),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if is_transient(&e) => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e),
        }
    }
}

fn is_transient(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::TimedOut
            | io::ErrorKind::OutOfMemory
    ) || matches!(
        error.raw_os_error(),
        Some(libc::EMFILE | libc::ENFILE | libc::ENOBUFS | libc::ENOMEM)
    )
}

/// One connection's slot under [`MAX_CONNECTIONS`] and [`MAX_PEER_CONNECTIONS`].
struct Active {
    shared: Arc<Shared>,
    peer: IpAddr,
}

impl Drop for Active {
    fn drop(&mut self) {
        {
            let mut peers = self.shared.lock_peers();
            if let Some(open) = peers.get_mut(&self.peer) {
                *open = open.saturating_sub(1);
                if *open == 0 {
                    peers.remove(&self.peer);
                }
            }
        }
        self.shared.active.fetch_sub(1, Ordering::SeqCst);
    }
}

fn dispatch(
    shared: &Arc<Shared>,
    stream: TcpStream,
    peer: IpAddr,
    refusals: &SyncSender<TcpStream>,
) {
    let peer = peer.to_canonical();
    if shared.active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
        shared.active.fetch_sub(1, Ordering::SeqCst);
        refuse(
            stream,
            format!("the server is at its {MAX_CONNECTIONS} connections; try again"),
            refusals,
        );
        return;
    }
    let admitted = {
        let mut peers = shared.lock_peers();
        let open = peers.entry(peer).or_insert(0);
        let admitted = *open < MAX_PEER_CONNECTIONS;
        if admitted {
            *open += 1;
        }
        admitted
    };
    if !admitted {
        shared.active.fetch_sub(1, Ordering::SeqCst);
        refuse(
            stream,
            format!(
                "too many connections from your address (at most {MAX_PEER_CONNECTIONS}); try again"
            ),
            refusals,
        );
        return;
    }
    #[cfg(test)]
    shared.served.fetch_add(1, Ordering::SeqCst);
    let active = Active {
        shared: Arc::clone(shared),
        peer,
    };
    let spawned = std::thread::Builder::new()
        .name("vault-conn".to_owned())
        .stack_size(CONNECTION_STACK)
        .spawn(move || {
            handle(&active.shared, stream, active.peer);
            drop(active);
        });
    drop(spawned);
}

/// Answer a connection over a cap with 503 from the accept loop ([`linger`]).
fn refuse(mut stream: TcpStream, why: String, refusals: &SyncSender<TcpStream>) {
    let sent = write_reply(
        &mut stream,
        &Reply::text(503, why),
        REFUSAL_DEADLINE,
        REFUSAL_DEADLINE,
    );
    if sent.is_ok() {
        let _ = refusals.try_send(stream);
    }
}

/// The PUT bodies being read now.
#[derive(Default)]
struct Puts {
    reading: usize,
    by_peer: HashMap<IpAddr, usize>,
}

/// One PUT body being read under [`MAX_PUTS`] and [`MAX_PEER_PUTS`], released when dropped.
struct Reading<'a> {
    shared: &'a Shared,
    peer: IpAddr,
    /// From this machine.
    local: bool,
}

impl<'a> Reading<'a> {
    /// A slot to read a PUT body from `peer`, or the 503 that says which cap it met.
    fn take(shared: &'a Shared, peer: IpAddr, local: bool) -> Result<Reading<'a>, Reply> {
        let busy = |why: String| Err(Reply::text(503, why));
        let mut puts = shared.lock_puts();
        if puts.reading >= MAX_PUTS {
            return busy(format!(
                "the server is reading {MAX_PUTS} envelopes already; try again"
            ));
        }
        if !local {
            if puts.reading >= MAX_PUTS - 1 {
                return busy(format!(
                    "the server is reading {} envelopes from other machines already (the last slot is kept for its own); try again",
                    MAX_PUTS - 1
                ));
            }
            let from_peer = puts.by_peer.entry(peer).or_insert(0);
            if *from_peer >= MAX_PEER_PUTS {
                return busy(
                    "your address is sending an envelope already; send one at a time".to_owned(),
                );
            }
            *from_peer += 1;
        }
        puts.reading += 1;
        Ok(Reading {
            shared,
            peer,
            local,
        })
    }
}

impl Drop for Reading<'_> {
    fn drop(&mut self) {
        let mut puts = self.shared.lock_puts();
        puts.reading = puts.reading.saturating_sub(1);
        if !self.local
            && let Some(from_peer) = puts.by_peer.get_mut(&self.peer)
        {
            *from_peer = from_peer.saturating_sub(1);
            if *from_peer == 0 {
                puts.by_peer.remove(&self.peer);
            }
        }
    }
}

/// An answer.
struct Reply {
    status: u16,
    body: Body,
    etag: Option<u64>,
    allow: Option<&'static str>,
}

enum Body {
    Empty,
    Text(String),
    Envelope(Arc<Vec<u8>>),
}

impl Reply {
    fn text(status: u16, why: impl Into<String>) -> Reply {
        let mut text: String = why.into();
        text.push('\n');
        Reply {
            status,
            body: Body::Text(text),
            etag: None,
            allow: None,
        }
    }

    const fn empty(status: u16) -> Reply {
        Reply {
            status,
            body: Body::Empty,
            etag: None,
            allow: None,
        }
    }
}

const fn reason_phrase(status: u16) -> &'static str {
    match status {
        100 => "Continue",
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Content Too Large",
        417 => "Expectation Failed",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        505 => "HTTP Version Not Supported",
        507 => "Insufficient Storage",
        _ => "Unknown",
    }
}

/// Send `reply`, no write blocking longer than `stall` and the whole within `within`.
fn write_reply(
    stream: &mut TcpStream,
    reply: &Reply,
    stall: Duration,
    within: Duration,
) -> io::Result<()> {
    let deadline = Instant::now().checked_add(within);
    let body: &[u8] = match &reply.body {
        Body::Empty => &[],
        Body::Text(text) => text.as_bytes(),
        Body::Envelope(bytes) => bytes,
    };
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nConnection: close\r\nCache-Control: no-store\r\n",
        reply.status,
        reason_phrase(reply.status)
    );
    if let Some(seq) = reply.etag {
        head.push_str(&format!("ETag: \"{seq}\"\r\n"));
    }
    if let Some(allow) = reply.allow {
        head.push_str(&format!("Allow: {allow}\r\n"));
    }
    if reply.status != 204 {
        head.push_str("Content-Type: text/plain; charset=utf-8\r\n");
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");
    send(stream, head.as_bytes(), stall, deadline)?;
    if reply.status != 204 {
        send(stream, body, stall, deadline)?;
    }
    stream.flush()
}

/// Write all of `bytes`, no write blocking longer than `stall` and none past `deadline`.
fn send(
    stream: &mut TcpStream,
    mut bytes: &[u8],
    stall: Duration,
    deadline: Option<Instant>,
) -> io::Result<()> {
    while !bytes.is_empty() {
        let wait = match deadline {
            Some(deadline) => deadline
                .checked_duration_since(Instant::now())
                .filter(|left| !left.is_zero())
                .ok_or(io::ErrorKind::TimedOut)?
                .min(stall),
            None => stall,
        };
        stream.set_write_timeout(Some(wait))?;
        match stream.write(&bytes[..bytes.len().min(WRITE_CHUNK)]) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// One request's head.
struct Request {
    method: String,
    path: String,
    query: Option<String>,
    length: Option<usize>,
    expect_continue: bool,
    /// Body bytes that arrived with the head.
    early: Vec<u8>,
}

fn handle(shared: &Shared, mut stream: TcpStream, peer: IpAddr) {
    let _ = stream.set_nodelay(true);
    let tuning = shared.tuning;
    let reply = match read_request(&mut stream, tuning.head_deadline) {
        Ok(Some(request)) => respond(shared, &mut stream, peer, request),
        Ok(None) => return,
        Err(reply) => reply,
    };
    if write_reply(
        &mut stream,
        &reply,
        tuning.write_stall,
        tuning.answer_deadline,
    )
    .is_ok()
    {
        linger(&mut stream, tuning.linger);
    }
}

/// Close our half and drain what the client still sends, for at most `deadline`, so a client
/// still writing (an oversize body) reads the answer instead of a reset.
fn linger(stream: &mut TcpStream, deadline: Duration) {
    if stream.shutdown(Shutdown::Write).is_err() {
        return;
    }
    let started = Instant::now();
    let mut drained = 0usize;
    let mut sink = [0u8; 8192];
    while drained < LINGER_BYTES {
        let Some(left) = deadline
            .checked_sub(started.elapsed())
            .filter(|left| !left.is_zero())
        else {
            return;
        };
        if stream.set_read_timeout(Some(left)).is_err() {
            return;
        }
        match stream.read(&mut sink) {
            Ok(0) | Err(_) => return,
            Ok(n) => drained += n,
        }
    }
}

fn find_blank_line(buf: &[u8], from: usize) -> Option<usize> {
    buf.get(from..)?
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|at| from + at + 4)
}

/// Read and parse one head within `deadline` ([`HEAD_DEADLINE`]) and [`MAX_HEAD`].
fn read_request(stream: &mut TcpStream, deadline: Duration) -> Result<Option<Request>, Reply> {
    let late = || {
        Reply::text(
            408,
            format!("the request head took longer than {deadline:?}"),
        )
    };
    let started = Instant::now();
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    let end = loop {
        if let Some(end) = find_blank_line(&buf, buf.len().saturating_sub(chunk.len() + 3)) {
            break end;
        }
        if buf.len() >= MAX_HEAD {
            return Err(Reply::text(
                431,
                format!("the request head is larger than {MAX_HEAD} bytes"),
            ));
        }
        let Some(left) = deadline
            .checked_sub(started.elapsed())
            .filter(|left| !left.is_zero())
        else {
            return Err(late());
        };
        if stream.set_read_timeout(Some(left)).is_err() {
            return Ok(None);
        }
        match stream.read(&mut chunk) {
            Ok(0) if buf.is_empty() => return Ok(None),
            Ok(0) => return Err(Reply::text(400, "the request ended inside its head")),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(late());
            }
            Err(_) => return Ok(None),
        }
    };
    if end > MAX_HEAD {
        return Err(Reply::text(
            431,
            format!("the request head is larger than {MAX_HEAD} bytes"),
        ));
    }
    let early = buf.split_off(end);
    parse_head(&buf[..end - 4], early).map(Some)
}

fn parse_head(head: &[u8], early: Vec<u8>) -> Result<Request, Reply> {
    let bad = |why: &str| Reply::text(400, why);
    let head = std::str::from_utf8(head).map_err(|_| bad("the request head is not text"))?;
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let parts: Vec<&str> = request_line.split(' ').collect();
    let [method, target, version] = parts.as_slice() else {
        return Err(bad("the request line is not `METHOD /path HTTP/1.1`"));
    };
    if method.is_empty() || !method.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(bad("the request method is not a method"));
    }
    if !target.starts_with('/') || !target.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(bad("the request target is not an absolute path"));
    }
    if !matches!(*version, "HTTP/1.1" | "HTTP/1.0") {
        return Err(Reply::text(505, "this server speaks HTTP/1.1"));
    }
    let mut length: Option<usize> = None;
    let mut expect_continue = false;
    for line in lines {
        if line.starts_with([' ', '\t']) {
            return Err(bad("folded header lines are not accepted"));
        }
        if line.contains(['\r', '\n']) {
            return Err(bad("header lines end in CRLF"));
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| bad("a header line has no colon"))?;
        if name.is_empty() || !name.bytes().all(token_char) {
            return Err(bad("a header name is not a token"));
        }
        if !value
            .bytes()
            .all(|b| b == b'\t' || (0x20..0x7f).contains(&b) || b >= 0x80)
        {
            return Err(bad("a header value holds a control character"));
        }
        let value = value.trim_matches([' ', '\t']);
        if browser_metadata_header(name) {
            return Err(Reply::text(403, BROWSER_REFUSED));
        }
        if name.eq_ignore_ascii_case("content-length") {
            let parsed = (!value.is_empty()
                && value.len() <= 12
                && value.bytes().all(|b| b.is_ascii_digit()))
            .then(|| value.parse::<usize>().ok())
            .flatten()
            .ok_or_else(|| bad("Content-Length is not a byte count"))?;
            if length.is_some_and(|first| first != parsed) {
                return Err(bad("conflicting Content-Length headers"));
            }
            length = Some(parsed);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(Reply::text(
                501,
                "chunked bodies are not accepted; send Content-Length",
            ));
        } else if name.eq_ignore_ascii_case("expect") {
            if value.eq_ignore_ascii_case("100-continue") {
                expect_continue = true;
            } else {
                return Err(Reply::text(417, "the only expectation met is 100-continue"));
            }
        }
    }
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path, Some(query.to_owned())),
        None => (*target, None),
    };
    Ok(Request {
        method: (*method).to_owned(),
        path: path.to_owned(),
        query,
        length,
        expect_continue,
        early,
    })
}

/// Why a request carrying browser metadata is refused.
const BROWSER_REFUSED: &str =
    "browser-origin or fetch-metadata headers are not accepted (Origin or Sec-Fetch-*)";

/// Reject browser request metadata before processing a body or admitting a join.
fn browser_metadata_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("origin")
        || name
            .get(..10)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("sec-fetch-"))
}

fn token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// A path this server answers.
enum Route<'a> {
    Vault(&'a str),
    Join(&'a str),
}

/// `/v1/<name>` or `/v1/<name>/join`, with `<name>` a vault name.
fn route(path: &str) -> Option<Route<'_>> {
    let rest = path.strip_prefix("/v1/")?;
    let (name, join) = match rest.split_once('/') {
        None => (rest, false),
        Some((name, "join")) => (name, true),
        Some(_) => return None,
    };
    if !envelope::valid_name(name) {
        return None;
    }
    Some(if join {
        Route::Join(name)
    } else {
        Route::Vault(name)
    })
}

fn respond(shared: &Shared, stream: &mut TcpStream, peer: IpAddr, mut request: Request) -> Reply {
    let path = std::mem::take(&mut request.path);
    let Some(route) = route(&path) else {
        return Reply::text(
            404,
            "no such path: the vault paths are /v1/<name> and /v1/<name>/join",
        );
    };
    let method = request.method.as_str();
    if !matches!(
        (&route, method),
        (Route::Vault(_), "GET" | "PUT") | (Route::Join(_), "GET" | "POST")
    ) {
        let allow = match route {
            Route::Vault(_) => "GET, PUT",
            Route::Join(_) => "GET, POST",
        };
        let mut reply = Reply::text(405, format!("{method} is not answered here; use {allow}"));
        reply.allow = Some(allow);
        return reply;
    }
    if method == "GET" && (request.length.unwrap_or(0) > 0 || !request.early.is_empty()) {
        return Reply::text(400, "a GET carries no body");
    }
    let vault_get = matches!((&route, method), (Route::Vault(_), "GET"));
    if request.query.is_some() && !vault_get {
        return Reply::text(400, "this path takes no query");
    }
    match (route, method) {
        (Route::Vault(name), "GET") => get_vault(shared, stream, name, request.query.as_deref()),
        (Route::Vault(name), _) => {
            let length = match body_length(&request, MAX_ENVELOPE) {
                Ok(length) => length,
                Err(reply) => return reply,
            };
            let arrived_on = stream.local_addr().ok().map(|addr| addr.ip());
            let local = (shared.tuning.is_local)(peer, arrived_on);
            let refused = refuse_unheld(shared, &shared.lock_server_state(), name, local);
            if let Some(reply) = refused {
                return reply;
            }
            let _reading = match Reading::take(shared, peer, local) {
                Ok(reading) => reading,
                Err(reply) => return reply,
            };
            match read_body(stream, request, length, &shared.tuning) {
                Ok(body) => put_vault(shared, name, &body, local),
                Err(reply) => reply,
            }
        }
        (Route::Join(name), "GET") => list_joins(shared, name),
        (Route::Join(name), _) => match body_length(&request, MAX_JOIN_BODY)
            .and_then(|length| read_body(stream, request, length, &shared.tuning))
        {
            Ok(body) => post_join(shared, name, &body, peer),
            Err(reply) => reply,
        },
    }
}

/// Why a PUT for `name` is refused whatever it carries, when the store holds nothing for `name`.
fn refuse_unheld(shared: &Shared, state: &State, name: &str, local: bool) -> Option<Reply> {
    if state.vaults.contains_key(name) {
        return None;
    }
    if !local && !shared.pins.iter().any(|(pinned, _)| pinned == name) {
        return Some(Reply::text(
            403,
            "the first version of a vault is accepted only from the server's own Mac, or under aterm secrets serve --pin <vault>=<key>",
        ));
    }
    if state.vaults.len() >= MAX_VAULTS {
        return Some(Reply::text(
            507,
            format!("this server holds its limit of {MAX_VAULTS} vaults"),
        ));
    }
    None
}

/// A body's `Content-Length`.
fn body_length(request: &Request, max: usize) -> Result<usize, Reply> {
    let Some(length) = request.length else {
        return Err(Reply::text(
            411,
            "a body needs Content-Length (chunked is not accepted)",
        ));
    };
    if length > max {
        return Err(Reply::text(
            413,
            format!("the body is larger than {max} bytes"),
        ));
    }
    if request.early.len() > length {
        return Err(Reply::text(
            400,
            "bytes follow the body (one request per connection)",
        ));
    }
    Ok(length)
}

/// Read the `length` bytes of body [`body_length`] admitted, within the body deadline and stall.
fn read_body(
    stream: &mut TcpStream,
    request: Request,
    length: usize,
    tuning: &Tuning,
) -> Result<Vec<u8>, Reply> {
    let mut body = request.early;
    if request.expect_continue && body.len() < length {
        let continued = stream
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .and_then(|()| stream.flush());
        if continued.is_err() {
            return Err(Reply::text(400, "the connection closed"));
        }
    }
    let started = Instant::now();
    let mut chunk = vec![0u8; 64 * 1024];
    while body.len() < length {
        let Some(left) = tuning
            .body_deadline
            .checked_sub(started.elapsed())
            .filter(|left| !left.is_zero())
        else {
            return Err(Reply::text(408, "the body took too long"));
        };
        let wait = left.min(tuning.body_stall);
        if stream.set_read_timeout(Some(wait)).is_err() {
            return Err(Reply::text(400, "the connection closed"));
        }
        let want = (length - body.len()).min(chunk.len());
        match stream.read(&mut chunk[..want]) {
            Ok(0) => return Err(Reply::text(400, "the body ended before its Content-Length")),
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                let why = if wait == left {
                    "the body took too long"
                } else {
                    "the body stalled"
                };
                return Err(Reply::text(408, why));
            }
            Err(_) => return Err(Reply::text(400, "the connection closed")),
        }
    }
    Ok(body)
}

fn envelope_reply(held: &Held) -> Reply {
    Reply {
        status: 200,
        body: Body::Envelope(Arc::clone(&held.bytes)),
        etag: Some(held.seq),
        allow: None,
    }
}

fn get_vault(shared: &Shared, stream: &TcpStream, name: &str, query: Option<&str>) -> Reply {
    let after = match query {
        None => None,
        Some(query) => match query.strip_prefix("after=") {
            Some(digits)
                if !digits.is_empty()
                    && digits.len() <= 20
                    && digits.bytes().all(|b| b.is_ascii_digit()) =>
            {
                match digits.parse::<u64>() {
                    Ok(after) => Some(after),
                    Err(_) => return Reply::text(400, "after= is not a seq"),
                }
            }
            _ => return Reply::text(400, "the only query is after=<seq>"),
        },
    };
    let mut state = shared.lock_server_state();
    let Some(slot) = state.vaults.get(name) else {
        return Reply::text(404, format!("no vault named {name} here"));
    };
    let Some(after) = after else {
        return envelope_reply(&slot.held);
    };
    if slot.held.seq > after {
        return envelope_reply(&slot.held);
    }
    let wake = Arc::clone(&slot.wake);
    let deadline = Instant::now().checked_add(shared.hold);
    let every = shared.tuning.liveness_every;
    #[cfg(test)]
    shared.waiting.fetch_add(1, Ordering::SeqCst);
    loop {
        let newer = state
            .vaults
            .get(name)
            .is_some_and(|slot| slot.held.seq > after);
        if newer || shared.stopping.load(Ordering::SeqCst) {
            break;
        }
        let slice = match deadline {
            Some(deadline) => match deadline
                .checked_duration_since(Instant::now())
                .filter(|left| !left.is_zero())
            {
                Some(left) => left.min(every),
                None => break,
            },
            None => every,
        };
        let (next, waited) = wake
            .wait_timeout(state, slice)
            .unwrap_or_else(PoisonError::into_inner);
        state = next;
        if waited.timed_out() && gone(stream) {
            break;
        }
    }
    #[cfg(test)]
    shared.waiting.fetch_sub(1, Ordering::SeqCst);
    match state.vaults.get(name) {
        Some(slot) if slot.held.seq > after => envelope_reply(&slot.held),
        Some(slot) => {
            let mut reply = Reply::empty(204);
            reply.etag = Some(slot.held.seq);
            reply
        }
        None => Reply::empty(204),
    }
}

/// Whether a held long-poll's client went away.
fn gone(stream: &TcpStream) -> bool {
    if stream.set_nonblocking(true).is_err() {
        return true;
    }
    let mut byte = [0u8; 1];
    let gone = match stream.peek(&mut byte) {
        Ok(0) => true,
        Ok(_) => false,
        Err(e) => !matches!(
            e.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
        ),
    };
    stream.set_nonblocking(false).is_err() || gone
}

fn put_vault(shared: &Shared, name: &str, body: &[u8], local: bool) -> Reply {
    let envelope = match envelope::parse(body) {
        Ok(envelope) => envelope,
        Err(e) => return Reply::text(400, e.0),
    };
    if envelope.name != name {
        return Reply::text(
            400,
            format!(
                "the envelope names the vault `{}`, not `{name}`",
                envelope.name
            ),
        );
    }
    if let Err(e) = envelope.verify_signature() {
        return Reply::text(403, e.0);
    }
    if let Err(e) = envelope.check_signer(&shared.retired) {
        return Reply::text(403, e.0);
    }
    let mut state = shared.lock_server_state();
    let mut pin = envelope::Pin::Same;
    match state.vaults.get(name) {
        Some(slot) => {
            let held = &slot.held;
            let followed: Vec<[u8; 32]> = held.handoff.iter().map(|(key, _)| *key).collect();
            pin = match envelope.check_pin_with(&held.signer, &followed, &shared.retired) {
                Ok(pin) => pin,
                Err(e) => return Reply::text(403, e.0),
            };
            if !matches!(pin, envelope::Pin::Replaced(_)) {
                if envelope.seq <= held.seq {
                    if envelope.seq == held.seq && held.bytes.as_slice() == body {
                        return Reply::empty(204);
                    }
                    return Reply::text(
                        409,
                        format!(
                            "seq {} is not newer than seq {}, which this server holds",
                            envelope.seq, held.seq
                        ),
                    );
                }
                if let Err(e) = envelope.check_extends(&held.signer, &held.handoff) {
                    return Reply::text(403, e.0);
                }
            }
        }
        None => {
            if let Some((_, configured)) = shared.pins.iter().find(|(pinned, _)| pinned == name) {
                pin = match envelope.check_pin_with(configured, &[], &shared.retired) {
                    Ok(pin) => pin,
                    Err(e) => return Reply::text(403, e.0),
                };
            }
            if let Some(reply) = refuse_unheld(shared, &state, name, local) {
                return reply;
            }
        }
    }
    if let Err(e) = fsx::write_atomic(&shared.vault_path(name), body, 0o600) {
        note(&format!(
            "{name}: could not store seq {}: {e}",
            envelope.seq
        ));
        return Reply::text(500, "the server could not store the envelope");
    }
    let held = Held {
        seq: envelope.seq,
        signer: envelope.signer,
        handoff: envelope.handoff.clone(),
        bytes: Arc::new(body.to_vec()),
    };
    let first = !state.vaults.contains_key(name);
    let slot = state.vaults.entry(name.to_owned()).or_insert_with(|| Slot {
        held: Held {
            seq: 0,
            signer: envelope.signer,
            handoff: Vec::new(),
            bytes: Arc::new(Vec::new()),
        },
        wake: Arc::new(Condvar::new()),
    });
    slot.held = held;
    slot.wake.notify_all();
    note(&format!(
        "{name}: accepted seq {} signed by {}{}",
        envelope.seq,
        envelope::fingerprint(&envelope.signer),
        match pin {
            envelope::Pin::Rotated(_) => " (a handoff from the pinned key)",
            envelope::Pin::Replaced(_) =>
                " (the pinned key gave way to the key this build names, and what it held with it)",
            envelope::Pin::Same if first => " (its first envelope here)",
            envelope::Pin::Same => "",
        }
    ));
    Reply::empty(204)
}

fn post_join(shared: &Shared, name: &str, body: &[u8], peer: IpAddr) -> Reply {
    let Ok(text) = std::str::from_utf8(body) else {
        return Reply::text(400, "a join request is one line of text");
    };
    let line = text.strip_suffix('\n').unwrap_or(text);
    if line.contains(['\n', '\r']) {
        return Reply::text(400, "a join request is one line: `age1… <host> <code>`");
    }
    let request = match parse_join_line(line) {
        Ok(request) if request.time.is_none() => request,
        Ok(_) => return Reply::text(400, "a join request is `age1… <host> <code>`"),
        Err(e) => return Reply::text(400, e.0),
    };
    let code = crate::age::Recipient::parse(&request.recipient)
        .ok()
        .map(|key| crate::client::join_code(&key));
    if code.as_deref() != Some(request.code.as_str()) {
        return Reply::text(400, "a join request's code is not its key's");
    }
    let now = (shared.tuning.now)();
    let _writing = shared.join_writes();
    let (queue, noted) = match decide_join(shared, name, request, peer, now) {
        Ok(Decided::Next(queue, noted)) => (queue, noted),
        Ok(Decided::Queued) => return Reply::empty(204),
        Err(reply) => return reply,
    };
    if let Err(e) = fsx::write_atomic(
        &shared.joins_path(name),
        stored_lines(&queue).as_bytes(),
        0o600,
    ) {
        note(&format!("{name}: could not store a join request: {e}"));
        return Reply::text(500, "the server could not store the request");
    }
    shared
        .lock_server_state()
        .joins
        .insert(name.to_owned(), queue);
    if let Some(line) = noted {
        note(&format!("{name}: {line}"));
    }
    Reply::empty(204)
}

/// What [`decide_join`] decided.
enum Decided {
    /// The request is queued as it is already ([`Queued::asked`]).
    Queued,
    /// The queue to store, and the line to log.
    Next(Vec<Queued>, Option<String>),
}

/// Decide a join request from `peer` under `server_state`, or refuse it ([`Shared::join_writes`]).
fn decide_join(
    shared: &Shared,
    name: &str,
    request: JoinRequest,
    peer: IpAddr,
    now: u64,
) -> Result<Decided, Reply> {
    let mut state = shared.lock_server_state();
    let Some(seq) = state.vaults.get(name).map(|slot| slot.held.seq) else {
        return Err(Reply::text(404, format!("no vault named {name} here")));
    };
    let mut queue = state.joins.get(name).cloned().unwrap_or_default();
    expire(&mut queue, now);
    let same = |queued: &Queued| {
        let asked = &queued.request;
        asked.recipient == request.recipient
            && asked.host == request.host
            && asked.code == request.code
            && asked.proof == request.proof
    };
    if let Some(at) = queue.iter().position(same) {
        if now.saturating_sub(queue[at].request.time.unwrap_or(0)) < JOIN_REFRESH_EVERY_SECS {
            if let Some(held) = state
                .joins
                .get_mut(name)
                .and_then(|queue| queue.iter_mut().find(|queued| same(queued)))
            {
                held.asked = seq;
            }
            return Ok(Decided::Queued);
        }
        queue[at].request.time = Some(now);
        queue[at].asked = seq;
        return Ok(Decided::Next(queue, None));
    }
    let ours = |queued: &Queued| {
        queued.request.recipient == request.recipient && queued.peer.is_none_or(|at| at == peer)
    };
    let previous = queue.iter().position(ours).map(|at| queue.remove(at));
    if let Some(previous) = &previous {
        let gap = shared.tuning.join_change_gap;
        if now
            .checked_sub(previous.changed)
            .is_some_and(|since| since < gap.as_secs())
        {
            return Err(Reply::text(
                429,
                format!("this key's join request changed less than {gap:?} ago; ask again shortly"),
            ));
        }
    }
    if !make_room(
        &mut queue,
        MAX_PEER_JOINS,
        |queued| queued.peer == Some(peer),
        seq,
    ) {
        return Err(Reply::text(
            429,
            format!("your address already has {MAX_PEER_JOINS} join requests queued here"),
        ));
    }
    if !make_room(&mut queue, MAX_JOINS, |_| true, seq) {
        return Err(Reply::text(
            429,
            format!("the join queue already holds {MAX_JOINS} requests"),
        ));
    }
    let host = &request.host;
    let (noted, line) = match &previous {
        None => (now, Some(format!("join request queued for host {host}"))),
        Some(previous) if now.saturating_sub(previous.noted) >= JOIN_NOTE_EVERY_SECS => (
            now,
            Some(format!(
                "join request changed for host {host} (changes to one are noted at most hourly)"
            )),
        ),
        Some(previous) => (previous.noted, None),
    };
    queue.push(Queued {
        request: JoinRequest {
            time: Some(now),
            ..request
        },
        peer: Some(peer),
        asked: seq,
        changed: now,
        noted,
    });
    Ok(Decided::Next(queue, line))
}

/// Make room in `queue` for one more request among those `counted` picks, once they number `cap`.
fn make_room(
    queue: &mut Vec<Queued>,
    cap: usize,
    counted: impl Fn(&Queued) -> bool,
    seq: u64,
) -> bool {
    if queue.iter().filter(|queued| counted(queued)).count() < cap {
        return true;
    }
    match queue
        .iter()
        .position(|queued| counted(queued) && queued.asked != seq)
    {
        Some(at) => {
            queue.remove(at);
            true
        }
        None => false,
    }
}

fn list_joins(shared: &Shared, name: &str) -> Reply {
    let now = (shared.tuning.now)();
    let mut state = shared.lock_server_state();
    if !state.vaults.contains_key(name) {
        return Reply::text(404, format!("no vault named {name} here"));
    }
    let queue = state.joins.entry(name.to_owned()).or_default();
    expire(queue, now);
    Reply {
        status: 200,
        body: Body::Text(join_lines(queue)),
        etag: None,
        allow: None,
    }
}

/// Arguments to `dns-sd` registering `<name>.local` at `ip` and the vault service on `port`.
pub fn dns_sd_args(name: &str, port: u16, ip: Ipv4Addr) -> Vec<String> {
    vec![
        "-P".to_owned(),
        name.to_owned(),
        SERVICE_TYPE.to_owned(),
        "local".to_owned(),
        port.to_string(),
        format!("{name}.local"),
        ip.to_string(),
        "path=/v1".to_owned(),
    ]
}

/// The argv (after the program) that keeps the Mac from idle sleep while `pid` lives.
pub fn caffeinate_args(pid: u32) -> Vec<String> {
    vec!["-i".to_owned(), "-w".to_owned(), pid.to_string()]
}

/// The addresses this Mac's traffic leaves by: the default route's over IPv4, the LAN's and the
/// default route's over IPv6, each `None` without a route.
pub fn route_sources() -> [Option<IpAddr>; 3] {
    [
        primary_ipv4().map(IpAddr::V4),
        lan_ipv4().map(IpAddr::V4),
        primary_ipv6().map(IpAddr::V6),
    ]
}

/// Primary IPv4 address of the default-route interface.
pub fn primary_ipv4() -> Option<Ipv4Addr> {
    source_ipv4(Ipv4Addr::new(192, 0, 2, 1), 9)
}

/// LAN IPv4 address at which `<name>.local` is advertised ([`primary_ipv4`]).
fn lan_ipv4() -> Option<Ipv4Addr> {
    source_ipv4(Ipv4Addr::new(224, 0, 0, 251), 5353).or_else(primary_ipv4)
}

/// The local address a UDP socket connected to `to` gets (no packet is sent).
fn source_ipv4(to: Ipv4Addr, port: u16) -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((to, port)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_unspecified() && !ip.is_loopback() => Some(ip),
        _ => None,
    }
}

/// The IPv6 address the default route leaves by, found as [`primary_ipv4`] finds its own.
fn primary_ipv6() -> Option<Ipv6Addr> {
    let socket = UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).ok()?;
    socket
        .connect((Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1), 9))
        .ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V6(ip) if !ip.is_unspecified() && !ip.is_loopback() => Some(ip),
        _ => None,
    }
}

/// The children [`serve`] holds, killed when it is dropped.
#[derive(Default)]
struct Children(Arc<Mutex<ChildSet>>);

#[derive(Default)]
struct ChildSet {
    closed: bool,
    caffeinate: Option<aterm_uds::spawnfd::Child>,
    dns_sd: Option<(aterm_uds::spawnfd::Child, Ipv4Addr)>,
}

fn quiet(program: &str, args: &[String]) -> io::Result<aterm_uds::spawnfd::Child> {
    use aterm_uds::spawnfd::Stdio;
    aterm_uds::spawnfd::command(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

fn end(child: &mut aterm_uds::spawnfd::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

impl Children {
    fn lock(&self) -> MutexGuard<'_, ChildSet> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn keep_awake(&self) {
        if !cfg!(target_os = "macos") {
            note("keep-awake is macOS only; ignored");
            return;
        }
        match quiet(CAFFEINATE, &caffeinate_args(std::process::id())) {
            Ok(child) => self.lock().caffeinate = Some(child),
            Err(e) => note(&format!("could not start {CAFFEINATE}: {e}")),
        }
    }

    fn advertise(&self, name: &str, port: u16) {
        if !cfg!(target_os = "macos") {
            note("--advertise is macOS only (dns-sd); ignored");
            return;
        }
        let held = Arc::clone(&self.0);
        let name = name.to_owned();
        let spawned = std::thread::Builder::new()
            .name("vault-advertise".to_owned())
            .spawn(move || {
                loop {
                    {
                        let mut set = held.lock().unwrap_or_else(PoisonError::into_inner);
                        if set.closed {
                            return;
                        }
                        reregister(&mut set, &name, port, lan_ipv4());
                    }
                    std::thread::sleep(ADVERTISE_EVERY);
                }
            });
        if let Err(e) = spawned {
            note(&format!("could not start advertising: {e}"));
        }
    }
}

/// Keep one `dns-sd -P` registered at `ip`: a live registration at the same address stays.
fn reregister(set: &mut ChildSet, name: &str, port: u16, ip: Option<Ipv4Addr>) {
    if let (Some((child, held_ip)), Some(ip)) = (set.dns_sd.as_mut(), ip)
        && *held_ip == ip
        && matches!(child.try_wait(), Ok(None))
    {
        return;
    }
    if let Some((mut child, _)) = set.dns_sd.take() {
        end(&mut child);
    }
    let Some(ip) = ip else {
        return;
    };
    match quiet(DNS_SD, &dns_sd_args(name, port, ip)) {
        Ok(child) => {
            note(&format!("advertising {name}.local at {ip}:{port}"));
            set.dns_sd = Some((child, ip));
        }
        Err(e) => note(&format!("could not start {DNS_SD}: {e}")),
    }
}

impl Drop for Children {
    fn drop(&mut self) {
        let mut set = self.lock();
        set.closed = true;
        if let Some(mut child) = set.caffeinate.take() {
            end(&mut child);
        }
        if let Some((mut child, _)) = set.dns_sd.take() {
            end(&mut child);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{Locker, Unsigned, endorse, sign, signer_public, test_armor};
    use crate::transport::{self, Fetched, Source};

    const HANG_DETECTOR: Duration = Duration::from_secs(60);
    const OLD: [u8; 32] = [7u8; 32];
    const NEW: [u8; 32] = [8u8; 32];
    const STRANGER: [u8; 32] = [9u8; 32];

    fn envelope(seed: &[u8; 32], seq: u64) -> Vec<u8> {
        envelope_with(seed, seq, Vec::new(), "alab")
    }

    fn unsigned(seq: u64, handoff: Vec<([u8; 32], [u8; 64])>, name: &str) -> Unsigned {
        Unsigned {
            name: name.to_owned(),
            seq,
            time: 1_792_530_000 + seq,
            handoff,
            lockers: vec![
                Locker {
                    label: "admin".to_owned(),
                    armored: test_armor(&[seq as u8; 40]),
                },
                Locker {
                    label: "1".to_owned(),
                    armored: test_armor(b"everyone"),
                },
            ],
        }
    }

    fn envelope_with(
        seed: &[u8; 32],
        seq: u64,
        handoff: Vec<([u8; 32], [u8; 64])>,
        name: &str,
    ) -> Vec<u8> {
        sign(&unsigned(seq, handoff, name), seed).expect("signed")
    }

    fn options(store: &Path, hold: Duration) -> Options {
        Options {
            listen: "127.0.0.1:0".parse().expect("addr"),
            store: store.to_path_buf(),
            advertise: None,
            pins: Vec::new(),
            hold,
            keep_awake: false,
        }
    }

    fn run(options: &Options) -> Running {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        start(listener, options).expect("started")
    }

    fn run_tuned(options: &Options, tuning: Tuning) -> Running {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        launch(listener, options, tuning).expect("started")
    }

    /// Wait (a hang detector, not a budget) until `done` holds.
    fn until(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + HANG_DETECTOR;
        while !done() {
            assert!(Instant::now() < deadline, "never: {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// A connection that has sent `request` and reads nothing yet.
    fn sent(running: &Running, request: &[u8]) -> TcpStream {
        let mut stream = TcpStream::connect(running.addr).expect("connect");
        stream
            .set_read_timeout(Some(HANG_DETECTOR))
            .expect("timeout");
        stream.write_all(request).expect("write");
        stream
    }

    fn read_all(mut stream: TcpStream) -> String {
        let mut answer = Vec::new();
        let _ = stream.read_to_end(&mut answer);
        String::from_utf8_lossy(&answer).into_owned()
    }

    /// Key `n`'s join code.
    fn code(n: u8) -> String {
        let key = crate::transport::test_recipient(n);
        crate::client::join_code(&crate::age::Recipient::parse(&key).expect("a key"))
    }

    fn join_line(n: u8, host: &str) -> String {
        format!("{} {host} {}", crate::transport::test_recipient(n), code(n))
    }

    fn queued(request: JoinRequest, peer: Option<IpAddr>) -> Queued {
        Queued {
            request,
            peer,
            asked: 1,
            changed: 0,
            noted: 0,
        }
    }

    /// A server's clocks with no gap between changes to one join request.
    fn changeable() -> Tuning {
        Tuning {
            join_change_gap: Duration::ZERO,
            ..Tuning::default()
        }
    }

    /// Wait (a hang detector) until no connection is open.
    fn until_closed(running: &Running) {
        until("every connection has closed", || {
            running.shared.active.load(Ordering::SeqCst) == 0
        });
    }

    /// How many PUT bodies are being read.
    fn reading(running: &Running) -> usize {
        running.shared.lock_puts().reading
    }

    fn url(running: &Running) -> String {
        format!("http://{}/v1/alab", running.addr)
    }

    fn source(running: &Running) -> Source {
        Source::Url(url(running))
    }

    fn get(running: &Running) -> crate::Result<Fetched> {
        transport::fetch(&source(running), None, HANG_DETECTOR)
    }

    fn put(running: &Running, bytes: &[u8]) -> crate::Result<()> {
        transport::publish(&source(running), bytes)
    }

    /// Send `request` as is and read the whole answer.
    fn raw(running: &Running, request: &[u8]) -> String {
        let mut stream = TcpStream::connect(running.addr).expect("connect");
        stream
            .set_read_timeout(Some(HANG_DETECTOR))
            .expect("timeout");
        stream
            .set_write_timeout(Some(HANG_DETECTOR))
            .expect("timeout");
        stream.write_all(request).expect("write");
        let mut answer = Vec::new();
        let _ = stream.read_to_end(&mut answer);
        String::from_utf8_lossy(&answer).into_owned()
    }

    fn stored(store: &Path) -> Option<Vec<u8>> {
        std::fs::read(store.join("alab.vault")).ok()
    }

    /// Wait (a hang detector, not a budget) until `n` long-polls are held.
    fn until_waiting(running: &Running, n: usize) {
        let deadline = Instant::now() + HANG_DETECTOR;
        while running.waiting() != n {
            assert!(Instant::now() < deadline, "no long-poll was held");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn the_first_envelope_is_trusted_on_first_use_and_served_with_an_etag() {
        use std::os::unix::fs::PermissionsExt;
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let store = root.path().join("served");
        let running = run(&options(&store, Duration::from_millis(200)));
        assert_eq!(get(&running).expect("answered"), Fetched::Missing);

        let first = envelope(&OLD, 1);
        put(&running, &first).expect("trusted on first use");
        assert_eq!(get(&running).expect("held"), Fetched::Body(first.clone()));
        assert_eq!(stored(&store).as_deref(), Some(first.as_slice()));
        let mode =
            |path: &Path| std::fs::metadata(path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode(&store.join("alab.vault")), 0o600);
        assert_eq!(mode(&store), 0o700);

        let response = aterm_http::Client::new(
            aterm_http::Trust::PlatformVerifier,
            aterm_http::ProxyMode::Direct,
            HANG_DETECTOR,
        )
        .get(&url(&running))
        .limit(MAX_ENVELOPE)
        .send(&[])
        .expect("answered");
        assert_eq!(response.status(), 200);
        assert_eq!(response.header("etag"), Some("\"1\""));
        assert_eq!(response.header("cache-control"), Some("no-store"));
        assert_eq!(
            response.header("content-type"),
            Some("text/plain; charset=utf-8")
        );
        assert_eq!(response.body(), first.as_slice());

        put(&running, &first).expect("idempotent");
        running.stop();
    }

    #[test]
    fn a_store_that_holds_no_vault_says_a_mac_in_the_vault_puts_its_copy_here() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(
            &root.path().join("served"),
            Duration::from_millis(200),
        ));
        let pins = [("alab".to_string(), OLD)];
        let unheld =
            |pins: &[(String, [u8; 32])]| unheld_note(&running.shared.lock_server_state(), pins);
        let pinned = unheld(&pins).expect("an empty store says so");
        assert!(
            pinned.contains("a Mac in the vault puts its copy here"),
            "{pinned}"
        );
        assert!(
            pinned.contains("a vault this server pins (alab)"),
            "{pinned}"
        );
        let unpinned = unheld(&[]).expect("an empty store says so");
        assert!(unpinned.contains("only from this Mac"), "{unpinned}");
        put(&running, &envelope(&OLD, 1)).expect("trusted on first use");
        assert_eq!(unheld(&pins), None);
        running.stop();
    }

    #[test]
    fn refused_puts_name_why_and_leave_the_store_unchanged() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let store = root.path().to_path_buf();
        let running = run(&options(&store, Duration::from_millis(200)));
        let three = envelope(&OLD, 3);
        put(&running, &three).expect("first");

        let mut forged = envelope(&OLD, 4);
        let at = forged.len() - 10;
        forged[at] = if forged[at] == b'A' { b'B' } else { b'A' };
        let refusals = [
            (forged, "403", "signature"),
            (envelope(&OLD, 2), "409", "not newer"),
            (envelope(&OLD, 3)[..20].to_vec(), "400", "envelope"),
            (envelope(&STRANGER, 4), "403", "pinned key"),
            (
                envelope_with(&OLD, 4, Vec::new(), "other"),
                "400",
                "names the vault `other`",
            ),
        ];
        for (bytes, status, why) in refusals {
            let error = put(&running, &bytes).expect_err(why);
            assert!(error.0.contains(status), "{why}: {error}");
            assert!(error.0.contains(why), "{why}: {error}");
            assert_eq!(stored(&store).as_deref(), Some(three.as_slice()), "{why}");
            assert_eq!(get(&running).expect("held"), Fetched::Body(three.clone()));
        }
        running.stop();
    }

    #[test]
    fn a_same_seq_with_other_bytes_is_refused_as_not_newer() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        put(&running, &envelope(&OLD, 3)).expect("first");
        let other = sign(
            &Unsigned {
                name: "alab".to_owned(),
                seq: 3,
                time: 1,
                handoff: Vec::new(),
                lockers: Vec::new(),
            },
            &OLD,
        )
        .expect("signed");
        let error = put(&running, &other).expect_err("fork");
        assert!(error.0.contains("409"), "{error}");
        running.stop();
    }

    #[test]
    fn an_endorsed_handoff_moves_the_pin() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        put(&running, &envelope(&OLD, 1)).expect("first");
        let new = signer_public(&NEW).expect("public");
        let old = signer_public(&OLD).expect("public");
        let elsewhere = endorse(&OLD, "other", &new).expect("endorsed");
        let forged =
            crate::envelope::sign_unchecked(&unsigned(2, vec![(old, elsewhere)], "alab"), &NEW)
                .expect("rendered");
        let error = put(&running, &forged).expect_err("refused");
        assert!(error.0.contains("403"), "{error}");
        assert!(error.0.contains("endorsement"), "{error}");

        let chain = vec![(old, endorse(&OLD, "alab", &new).expect("e"))];
        let handoff = envelope_with(&NEW, 2, chain.clone(), "alab");
        put(&running, &handoff).expect("an endorsed handoff");
        put(&running, &envelope_with(&NEW, 3, chain, "alab")).expect("the new key");
        let error = put(&running, &envelope(&OLD, 4)).expect_err("the old key");
        assert!(error.0.contains("403"), "{error}");
        running.stop();
    }

    #[test]
    fn a_configured_pin_holds_for_a_vault_not_yet_held() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let mut options = options(root.path(), Duration::from_millis(200));
        options.pins = vec![("alab".to_owned(), signer_public(&OLD).expect("public"))];
        let running = run(&options);
        let error = put(&running, &envelope(&STRANGER, 1)).expect_err("not the pin");
        assert!(error.0.contains("403"), "{error}");
        assert!(stored(root.path()).is_none());
        put(&running, &envelope(&OLD, 1)).expect("the pin");
        running.stop();
    }

    #[test]
    fn a_fresh_store_pinned_to_the_first_key_takes_a_vault_three_rotations_on() {
        let seeds: Vec<[u8; 32]> = (0..4u8).map(|index| [0x40 + index; 32]).collect();
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let mut options = options(root.path(), Duration::from_millis(200));
        options.pins = vec![("alab".to_owned(), signer_public(&seeds[0]).expect("public"))];
        let running = run(&options);
        let mut broken = crate::envelope::test_chain("alab", &seeds);
        broken[1].1 = endorse(
            &seeds[1],
            "alab",
            &signer_public(&STRANGER).expect("public"),
        )
        .expect("endorsed");
        let forged = crate::envelope::sign_unchecked(&unsigned(9, broken, "alab"), &seeds[3])
            .expect("rendered");
        let error = put(&running, &forged).expect_err("a broken middle link");
        assert!(error.0.contains("403"), "{error}");
        assert!(error.0.contains("endorsement of handoff 2 of 3"), "{error}");
        let error = put(&running, &envelope(&seeds[3], 9)).expect_err("no chain");
        assert!(error.0.contains("403"), "{error}");
        assert!(stored(root.path()).is_none());

        let chained = envelope_with(
            &seeds[3],
            9,
            crate::envelope::test_chain("alab", &seeds),
            "alab",
        );
        put(&running, &chained).expect("followed from the pin");
        let error = put(&running, &envelope(&seeds[3], 10)).expect_err("the chain dropped");
        assert!(error.0.contains("403"), "{error}");
        assert!(error.0.contains("handoff 1 of 3"), "{error}");
        let next = envelope_with(
            &seeds[3],
            10,
            crate::envelope::test_chain("alab", &seeds),
            "alab",
        );
        put(&running, &next).expect("the chain carried on");
        running.stop();
    }

    #[test]
    fn a_rotation_that_drops_a_link_of_the_held_chain_is_refused() {
        let seeds: Vec<[u8; 32]> = (0..3u8).map(|index| [0x50 + index; 32]).collect();
        let chain = crate::envelope::test_chain("alab", &seeds);
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        put(&running, &envelope(&seeds[0], 1)).expect("first");
        let once = envelope_with(&seeds[1], 2, chain[..1].to_vec(), "alab");
        put(&running, &once).expect("the first rotation");

        let cut = envelope_with(&seeds[2], 3, chain[1..].to_vec(), "alab");
        let error = put(&running, &cut).expect_err("the first link dropped");
        assert!(error.0.contains("403"), "{error}");
        assert!(
            error.0.contains("drops or changes handoff 1 of 1"),
            "{error}"
        );
        let first = crate::envelope::fingerprint(&signer_public(&seeds[0]).expect("public"));
        assert!(error.0.contains(&first), "{error}");
        let error = put(&running, &envelope(&seeds[1], 3)).expect_err("dropped in place");
        assert!(error.0.contains("handoff 1 of 1"), "{error}");
        assert_eq!(stored(root.path()).as_deref(), Some(once.as_slice()));
        put(
            &running,
            &envelope_with(&seeds[2], 3, chain.clone(), "alab"),
        )
        .expect("the chain carried on");
        running.stop();

        let other = root.path().join("cut");
        let running = run(&options(&other, Duration::from_millis(200)));
        put(&running, &cut).expect("its first version, from this machine");
        put(&running, &envelope_with(&seeds[2], 4, chain, "alab")).expect("restored");
        running.stop();
    }

    #[test]
    fn a_store_the_removed_admin_fed_takes_the_vault_once_its_build_retires_the_key() {
        let old = signer_public(&OLD).expect("public");
        let to = |seed: &[u8; 32]| {
            vec![(
                old,
                endorse(&OLD, "alab", &signer_public(seed).expect("public")).expect("endorsed"),
            )]
        };
        let real = envelope_with(&NEW, 3, to(&NEW), "alab");
        let fork = envelope_with(&STRANGER, 1 << 40, to(&STRANGER), "alab");
        let high = envelope(&OLD, 1 << 40);
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let hold = Duration::from_millis(200);
        for (fed, why) in [(fork, "endorsed no handoff"), (high, "not newer")] {
            let store = root.path().join(why.replace(' ', "-"));
            let running = run(&options(&store, hold));
            put(&running, &envelope(&OLD, 1)).expect("first");
            put(&running, &fed).expect("taken before a build knew OLD retired");
            let error = put(&running, &real).expect_err("captured");
            assert!(error.0.contains(why), "{error}");
            running.stop();

            let running = run_tuned(&options(&store, hold), retiring());
            put(&running, &real).expect("the held key gave way");
            assert_eq!(stored(&store).as_deref(), Some(real.as_slice()));
            assert!(
                put(&running, &fed).is_err(),
                "what the removed admin signs is not taken again"
            );
            running.stop();
        }
    }

    #[test]
    fn joins_asked_under_a_key_that_gave_way_give_way_to_new_ones() {
        let old = signer_public(&OLD).expect("public");
        let real = envelope_with(
            &NEW,
            3,
            vec![(
                old,
                endorse(&OLD, "alab", &signer_public(&NEW).expect("public")).expect("endorsed"),
            )],
            "alab",
        );
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let hold = Duration::from_millis(200);
        let running = run(&options(root.path(), hold));
        put(&running, &envelope(&OLD, 1)).expect("first");
        put(&running, &envelope(&OLD, 1 << 40)).expect("the removed admin's");
        let ask = |running: &Running, n: u8| {
            transport::post_join(&url(running), &join_line(n, &format!("m{n}")))
        };
        for n in 1..=4 {
            ask(&running, n).expect("queued");
        }
        running.stop();

        let running = run_tuned(&options(root.path(), hold), retiring());
        put(&running, &real).expect("the held key gave way");
        ask(&running, 5).expect("m1 gave way");
        assert_eq!(
            transport::list_joins(&url(&running)).expect("listed").len(),
            4
        );
        running.stop();
        let running = run_tuned(&options(root.path(), hold), retiring());
        ask(&running, 6).expect("m2 gave way");
        running.stop();
    }

    /// A server that knows OLD retired, and NEW the key that replaced it.
    fn retiring() -> Tuning {
        Tuning {
            retired: || crate::envelope::Retired {
                keys: vec![signer_public(&OLD).expect("public")],
                current: Some(signer_public(&NEW).expect("public")),
            },
            ..Tuning::default()
        }
    }

    #[test]
    fn a_retired_key_signs_nothing_the_server_takes() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let hold = Duration::from_millis(200);
        let unaware = root.path().join("unaware");
        let running = run(&options(&unaware, hold));
        put(&running, &envelope(&OLD, 1)).expect("not retired here");
        running.stop();

        let empty = root.path().join("empty");
        let running = run_tuned(&options(&empty, hold), retiring());
        let error = put(&running, &envelope(&OLD, 1)).expect_err("retired");
        assert!(error.0.contains("403"), "{error}");
        assert!(error.0.contains("rotated away from"), "{error}");
        assert!(stored(&empty).is_none());
        running.stop();

        let running = run_tuned(&options(&unaware, hold), retiring());
        let error = put(&running, &envelope(&OLD, 2)).expect_err("retired");
        assert!(error.0.contains("rotated away from"), "{error}");
        let old = signer_public(&OLD).expect("public");
        let to = |seed: &[u8; 32]| endorse(&OLD, "alab", &signer_public(seed).expect("public"));
        let fork = envelope_with(
            &STRANGER,
            2,
            vec![(old, to(&STRANGER).expect("endorsed"))],
            "alab",
        );
        let error = put(&running, &fork).expect_err("a fork the old key endorsed");
        assert!(error.0.contains("403"), "{error}");
        let real = envelope_with(&NEW, 2, vec![(old, to(&NEW).expect("endorsed"))], "alab");
        put(&running, &real).expect("the key that replaced it");
        assert_eq!(stored(&unaware).as_deref(), Some(real.as_slice()));
        running.stop();
    }

    #[test]
    fn a_long_poll_answers_at_once_when_newer_and_wakes_on_a_put() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), HANG_DETECTOR));
        put(&running, &envelope(&OLD, 5)).expect("first");
        let poll = |after| transport::fetch(&source(&running), Some(after), HANG_DETECTOR);
        assert_eq!(poll(4).expect("newer"), Fetched::Body(envelope(&OLD, 5)));

        let target = source(&running);
        let waiter = std::thread::spawn(move || transport::fetch(&target, Some(5), HANG_DETECTOR));
        until_waiting(&running, 1);
        put(&running, &envelope(&OLD, 6)).expect("newer");
        let woke = waiter.join().expect("waiter");
        assert_eq!(woke.expect("woken"), Fetched::Body(envelope(&OLD, 6)));
        running.stop();
    }

    #[test]
    fn a_long_poll_ends_with_204_after_the_hold_or_at_stop() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        put(&running, &envelope(&OLD, 5)).expect("first");
        let fetched = transport::fetch(&source(&running), Some(5), HANG_DETECTOR).expect("held");
        assert_eq!(
            fetched,
            Fetched::NoNewer(Some(5)),
            "the 204 names the seq held"
        );
        running.stop();

        let running = run(&options(root.path(), HANG_DETECTOR));
        let target = source(&running);
        let waiter = std::thread::spawn(move || transport::fetch(&target, Some(5), HANG_DETECTOR));
        until_waiting(&running, 1);
        running.stop();
        assert_eq!(
            waiter.join().expect("waiter").expect("ended"),
            Fetched::NoNewer(Some(5))
        );
    }

    #[test]
    fn a_browsers_request_is_refused_before_anything_is_read_or_queued() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        put(&running, &envelope(&OLD, 1)).expect("first");
        let line = join_line(1, "m22");
        for header in [
            "Origin: https://example.com",
            "origin: null",
            "Sec-Fetch-Site: cross-site",
            "SEC-FETCH-MODE: no-cors",
        ] {
            let answer = raw(
                &running,
                format!(
                    "POST /v1/alab/join HTTP/1.1\r\nHost: alab-secrets.local:7484\r\n{header}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{line}",
                    line.len()
                )
                .as_bytes(),
            );
            assert!(answer.starts_with("HTTP/1.1 403 "), "{header}: {answer}");
        }
        for path in ["/v1/alab", "/v1/alab/join"] {
            let answer = raw(
                &running,
                format!(
                    "GET {path} HTTP/1.1\r\nHost: rebound.example:7484\r\nSec-Fetch-Site: same-origin\r\n\r\n"
                )
                .as_bytes(),
            );
            assert!(answer.starts_with("HTTP/1.1 403 "), "{path}: {answer}");
        }
        assert!(
            transport::list_joins(&url(&running))
                .expect("listed")
                .is_empty(),
            "nothing a browser sent was queued"
        );
        transport::post_join(&url(&running), &line).expect("queued");
        assert_eq!(
            transport::list_joins(&url(&running)).expect("listed").len(),
            1
        );
    }

    #[test]
    fn join_requests_queue_deduplicate_and_validate() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run_tuned(
            &options(root.path(), Duration::from_millis(200)),
            changeable(),
        );
        let key = |n: u8| crate::transport::test_recipient(n);
        let line = join_line(1, "m22");
        let error = transport::post_join(&url(&running), &line).expect_err("no vault yet");
        assert!(error.0.contains("404"), "{error}");
        put(&running, &envelope(&OLD, 1)).expect("first");

        transport::post_join(&url(&running), &line).expect("queued");
        transport::post_join(&url(&running), &join_line(2, "m3")).expect("queued");
        transport::post_join(&url(&running), &join_line(1, "m22b")).expect("again");
        let listed = transport::list_joins(&url(&running)).expect("listed");
        assert_eq!(listed.len(), 2, "{listed:?}");
        let requests: Vec<JoinRequest> = listed
            .iter()
            .map(|line| parse_join_line(line).expect("a listed line parses"))
            .collect();
        assert_eq!(requests[0].host, "m3");
        assert_eq!(requests[1].host, "m22b");
        assert!(requests.iter().all(|request| request.time.is_some()));

        for bad in [
            "nonsense".to_owned(),
            join_line(3, "M22"),
            format!("{} 1", join_line(3, "m22")),
            format!("{}\nsecond line", join_line(3, "m22")),
            format!("{} m22 {}", key(3), code(4)),
        ] {
            let answer = raw(
                &running,
                format!(
                    "POST /v1/alab/join HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{bad}",
                    bad.len()
                )
                .as_bytes(),
            );
            assert!(answer.starts_with("HTTP/1.1 400 "), "{bad:?}: {answer}");
        }
        assert_eq!(
            transport::list_joins(&url(&running)).expect("listed").len(),
            2
        );
        running.stop();
    }

    #[test]
    fn the_join_queue_is_bounded() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        put(&running, &envelope(&OLD, 1)).expect("first");
        let now = now();
        let mut queue = Vec::new();
        for n in 0..MAX_JOINS {
            let mut line = crate::transport::test_recipient(0);
            line.truncate(60);
            line.push(char::from(b"qpzry9x8gf2tvdw0s3jn54khce6mua7l"[n % 32]));
            line.push(char::from(b"qpzry9x8gf2tvdw0s3jn54khce6mua7l"[n / 32]));
            queue.push(queued(
                JoinRequest {
                    recipient: line,
                    host: format!("h{n}"),
                    code: "abcd-ef23-ghij-k234".to_owned(),
                    time: Some(now),
                    proof: None,
                },
                None,
            ));
        }
        running
            .shared
            .lock_server_state()
            .joins
            .insert("alab".to_owned(), queue);
        let error = transport::post_join(&url(&running), &join_line(1, "m22")).expect_err("full");
        assert!(error.0.contains("429"), "{error}");
        assert!(error.0.contains("already holds 256"), "{error}");

        put(&running, &envelope(&OLD, 2)).expect("a new version");
        transport::post_join(&url(&running), &join_line(1, "m22")).expect("room made");
        let state = running.shared.lock_server_state();
        let queue = &state.joins["alab"];
        assert_eq!(queue.len(), MAX_JOINS);
        assert_eq!(
            queue[0].request.host, "h1",
            "h0, the longest queued, gave way"
        );
        assert_eq!(queue[MAX_JOINS - 1].request.host, "m22");
        drop(state);
        running.stop();
    }

    #[test]
    fn the_server_restarts_from_its_store_and_expires_old_joins() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        put(&running, &envelope(&OLD, 4)).expect("first");
        let line = join_line(1, "m22");
        transport::post_join(&url(&running), &line).expect("queued");
        running.stop();

        let old = format!(
            "{} m9 abcd-ef24-ghij-k234 {}\n",
            crate::transport::test_recipient(2),
            now() - JOIN_TTL_SECS - 86_400
        );
        let joins = root.path().join("alab.joins");
        let mut text = std::fs::read_to_string(&joins).expect("joins");
        text.push_str(&old);
        fsx::write_atomic(&joins, text.as_bytes(), 0o600).expect("write");

        let running = run(&options(root.path(), Duration::from_millis(200)));
        assert_eq!(
            get(&running).expect("held"),
            Fetched::Body(envelope(&OLD, 4))
        );
        let listed = transport::list_joins(&url(&running)).expect("listed");
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert!(listed[0].starts_with(&line), "{listed:?}");
        assert!(put(&running, &envelope(&OLD, 3)).is_err());
        assert!(put(&running, &envelope(&STRANGER, 5)).is_err());
        put(&running, &envelope(&OLD, 5)).expect("newer");
        running.stop();
    }

    #[test]
    fn oversize_heads_and_bodies_are_refused() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        put(&running, &envelope(&OLD, 1)).expect("first");
        let mut huge = b"GET /v1/alab HTTP/1.1\r\nX-Pad: ".to_vec();
        huge.resize(MAX_HEAD + 1024, b'a');
        let answer = raw(&running, &huge);
        assert!(answer.starts_with("HTTP/1.1 431 "), "{answer}");

        let answer = raw(
            &running,
            format!(
                "PUT /v1/alab HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n",
                MAX_ENVELOPE + 1
            )
            .as_bytes(),
        );
        assert!(answer.starts_with("HTTP/1.1 413 "), "{answer}");
        let answer = raw(
            &running,
            b"POST /v1/alab/join HTTP/1.1\r\nHost: x\r\nContent-Length: 4096\r\n\r\n",
        );
        assert!(answer.starts_with("HTTP/1.1 413 "), "{answer}");
        let answer = raw(&running, b"PUT /v1/alab HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(answer.starts_with("HTTP/1.1 411 "), "{answer}");
        let answer = raw(
            &running,
            b"PUT /v1/alab HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
        );
        assert!(answer.starts_with("HTTP/1.1 501 "), "{answer}");
        assert_eq!(
            stored(root.path()).as_deref(),
            Some(envelope(&OLD, 1).as_slice())
        );
        running.stop();
    }

    #[test]
    fn an_expect_continue_put_is_answered_100_then_204() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        let body = envelope(&OLD, 1);
        let mut stream = TcpStream::connect(running.addr).expect("connect");
        stream
            .set_read_timeout(Some(HANG_DETECTOR))
            .expect("timeout");
        stream
            .write_all(
                format!(
                    "PUT /v1/alab HTTP/1.1\r\nHost: x\r\nExpect: 100-continue\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .expect("head");
        let mut interim = [0u8; 25];
        stream.read_exact(&mut interim).expect("100");
        assert_eq!(&interim, b"HTTP/1.1 100 Continue\r\n\r\n");
        stream.write_all(&body).expect("body");
        let mut answer = String::new();
        let _ = stream.read_to_string(&mut answer);
        assert!(answer.starts_with("HTTP/1.1 204 "), "{answer}");
        assert!(
            !answer.to_ascii_lowercase().contains("content-length"),
            "{answer}"
        );
        running.stop();
    }

    #[test]
    fn bad_paths_methods_and_queries_are_refused() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        put(&running, &envelope(&OLD, 1)).expect("first");
        for (request, status) in [
            ("GET /v1/../alab.vault HTTP/1.1", "404"),
            ("GET /v1/..%2Falab HTTP/1.1", "404"),
            ("GET /v1/Alab HTTP/1.1", "404"),
            ("GET /v1/alab/ HTTP/1.1", "404"),
            ("GET /v1/alab/join/x HTTP/1.1", "404"),
            ("GET /v2/alab HTTP/1.1", "404"),
            ("GET / HTTP/1.1", "404"),
            ("GET http://x/v1/alab HTTP/1.1", "400"),
            ("GET /v1/alab?after=x HTTP/1.1", "400"),
            ("GET /v1/alab?after=-1 HTTP/1.1", "400"),
            ("GET /v1/alab?before=1 HTTP/1.1", "400"),
            ("GET /v1/alab/join?x=1 HTTP/1.1", "400"),
            ("DELETE /v1/alab HTTP/1.1", "405"),
            ("POST /v1/alab HTTP/1.1", "405"),
            ("PUT /v1/alab/join HTTP/1.1", "405"),
            ("HEAD /v1/alab HTTP/1.1", "405"),
            ("GET /v1/alab HTTP/2.0", "505"),
            ("get /v1/alab HTTP/1.1", "400"),
            ("GET  /v1/alab HTTP/1.1", "400"),
        ] {
            let answer = raw(&running, format!("{request}\r\nHost: x\r\n\r\n").as_bytes());
            assert!(
                answer.starts_with(&format!("HTTP/1.1 {status} ")),
                "{request}: {answer}"
            );
            assert!(answer.contains("Connection: close\r\n"), "{answer}");
        }
        let answer = raw(&running, b"DELETE /v1/alab HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(answer.contains("Allow: GET, PUT\r\n"), "{answer}");
        let answer = raw(
            &running,
            b"GET /v1/alab HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\n\r\nhi",
        );
        assert!(answer.starts_with("HTTP/1.1 400 "), "{answer}");
        let answer = raw(&running, b"GET /v1/alab HTTP/1.1\r\n folded\r\n\r\n");
        assert!(answer.starts_with("HTTP/1.1 400 "), "{answer}");
        assert_eq!(
            get(&running).expect("held"),
            Fetched::Body(envelope(&OLD, 1))
        );
        running.stop();
    }

    #[test]
    fn the_children_argv_is_exact() {
        assert_eq!(
            dns_sd_args("alab-secrets", 7484, Ipv4Addr::new(192, 168, 1, 22)),
            [
                "-P",
                "alab-secrets",
                "_alab-vault._tcp",
                "local",
                "7484",
                "alab-secrets.local",
                "192.168.1.22",
                "path=/v1"
            ]
        );
        assert_eq!(caffeinate_args(4242), ["-i", "-w", "4242"]);
        assert_eq!(
            wake_addr("0.0.0.0:7484".parse().expect("addr")),
            "127.0.0.1:7484".parse().expect("addr")
        );
        assert_eq!(
            wake_addr("[::]:7484".parse().expect("addr")),
            "[::1]:7484".parse().expect("addr")
        );
        assert_eq!(
            wake_addr("10.0.0.2:7484".parse().expect("addr")),
            "10.0.0.2:7484".parse().expect("addr")
        );
    }

    #[test]
    fn expiry_keeps_a_week_either_side_of_the_clock() {
        let request = |time| {
            queued(
                JoinRequest {
                    recipient: crate::transport::test_recipient(1),
                    host: "m22".to_owned(),
                    code: "abcd-ef23-ghij-k234".to_owned(),
                    time: Some(time),
                    proof: None,
                },
                None,
            )
        };
        let now = 10 * JOIN_TTL_SECS;
        let mut queue = vec![
            request(now),
            request(now - JOIN_TTL_SECS),
            request(now - JOIN_TTL_SECS - 1),
            request(now + 2 * 86_400),
            request(now + JOIN_TTL_SECS),
            request(now + JOIN_TTL_SECS + 1),
        ];
        expire(&mut queue, now);
        let times: Vec<Option<u64>> = queue.iter().map(|queued| queued.request.time).collect();
        assert_eq!(
            times,
            [
                now,
                now - JOIN_TTL_SECS,
                now + 2 * 86_400,
                now + JOIN_TTL_SECS
            ]
            .map(Some)
        );
    }

    #[test]
    fn one_address_holds_at_most_16_connections_and_the_next_reads_503() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let tuning = Tuning {
            liveness_every: Duration::from_millis(10),
            ..Tuning::default()
        };
        let running = run_tuned(&options(root.path(), 2 * HANG_DETECTOR), tuning);
        put(&running, &envelope(&OLD, 1)).expect("first");
        until_closed(&running);
        let poll = b"GET /v1/alab?after=1 HTTP/1.1\r\nHost: x\r\n\r\n";
        let mut held: Vec<TcpStream> = (0..MAX_PEER_CONNECTIONS)
            .map(|_| sent(&running, poll))
            .collect();
        until_waiting(&running, MAX_PEER_CONNECTIONS);

        let answer = raw(&running, b"GET /v1/alab HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(answer.starts_with("HTTP/1.1 503 "), "{answer}");
        assert!(
            answer.contains("too many connections from your address"),
            "{answer}"
        );
        assert!(answer.contains("Connection: close\r\n"), "{answer}");

        drop(held.pop());
        until_waiting(&running, MAX_PEER_CONNECTIONS - 1);
        until("the departed client's slot is released", || {
            running
                .shared
                .lock_peers()
                .get(&IpAddr::V4(Ipv4Addr::LOCALHOST))
                .copied()
                == Some(MAX_PEER_CONNECTIONS - 1)
        });
        assert_eq!(
            get(&running).expect("served"),
            Fetched::Body(envelope(&OLD, 1))
        );
        running.stop();
        for stream in held {
            let answer = read_all(stream);
            assert!(answer.starts_with("HTTP/1.1 204 "), "{answer}");
        }
    }

    #[test]
    fn at_most_4_put_bodies_are_read_at_once() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let tuning = Tuning {
            body_deadline: 2 * HANG_DETECTOR,
            body_stall: 2 * HANG_DETECTOR,
            ..Tuning::default()
        };
        let running = run_tuned(&options(root.path(), Duration::from_millis(200)), tuning);
        let body = envelope(&OLD, 1);
        let head = format!(
            "PUT /v1/alab HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        let mut partial = head.clone().into_bytes();
        partial.extend_from_slice(&body[..10]);
        let stalled: Vec<TcpStream> = (0..MAX_PUTS).map(|_| sent(&running, &partial)).collect();
        until("four bodies are being read", || {
            reading(&running) == MAX_PUTS
        });

        let mut whole = head.into_bytes();
        whole.extend_from_slice(&body);
        let answer = raw(&running, &whole);
        assert!(answer.starts_with("HTTP/1.1 503 "), "{answer}");
        assert!(answer.contains("4 envelopes"), "{answer}");
        assert_eq!(get(&running).expect("answered"), Fetched::Missing);

        drop(stalled);
        until("the stalled bodies are let go", || reading(&running) == 0);
        put(&running, &body).expect("read now");
        running.stop();
    }

    /// Whether [`toggled`] says a peer is this machine's.
    static LOCAL: AtomicBool = AtomicBool::new(false);

    /// An `is_local` a test flips.
    fn toggled(_: IpAddr, _: Option<IpAddr>) -> bool {
        LOCAL.load(Ordering::SeqCst)
    }

    #[test]
    fn another_machine_reads_one_put_at_a_time_and_the_last_slot_is_this_machines() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let tuning = Tuning {
            body_deadline: 2 * HANG_DETECTOR,
            body_stall: 2 * HANG_DETECTOR,
            is_local: toggled,
            ..Tuning::default()
        };
        let mut options = options(root.path(), Duration::from_millis(200));
        options.pins = vec![("alab".to_owned(), signer_public(&OLD).expect("public"))];
        let running = run_tuned(&options, tuning);
        let body = envelope(&OLD, 1);
        let head = format!(
            "PUT /v1/alab HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        let mut partial = head.clone().into_bytes();
        partial.extend_from_slice(&body[..10]);
        let mut whole = head.into_bytes();
        whole.extend_from_slice(&body);

        LOCAL.store(false, Ordering::SeqCst);
        let mut stalled = vec![sent(&running, &partial)];
        until("its body is being read", || reading(&running) == 1);
        let answer = raw(&running, &whole);
        assert!(answer.starts_with("HTTP/1.1 503 "), "{answer}");
        assert!(answer.contains("your address is sending"), "{answer}");

        LOCAL.store(true, Ordering::SeqCst);
        stalled.extend((0..2).map(|_| sent(&running, &partial)));
        until("three bodies are being read", || reading(&running) == 3);
        LOCAL.store(false, Ordering::SeqCst);
        let answer = raw(&running, &whole);
        assert!(answer.starts_with("HTTP/1.1 503 "), "{answer}");
        assert!(answer.contains("from other machines"), "{answer}");
        LOCAL.store(true, Ordering::SeqCst);
        put(&running, &body).expect("this machine's PUT takes the last slot");

        drop(stalled);
        until("the stalled bodies are let go", || reading(&running) == 0);
        LOCAL.store(false, Ordering::SeqCst);
        put(&running, &envelope(&OLD, 2)).expect("another machine's, one at a time");
        assert!(running.shared.lock_puts().by_peer.is_empty());
        running.stop();
    }

    #[test]
    fn a_put_body_that_stalls_or_outruns_its_deadline_is_cut_with_408_and_frees_its_slot() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let stall = Tuning {
            body_stall: Duration::from_millis(100),
            body_deadline: 2 * HANG_DETECTOR,
            ..Tuning::default()
        };
        let deadline = Tuning {
            body_stall: 2 * HANG_DETECTOR,
            body_deadline: Duration::from_millis(200),
            linger: Duration::from_millis(100),
            ..Tuning::default()
        };
        let head = format!(
            "PUT /v1/alab HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\nabc",
            1 << 20
        );
        for (tuning, trickled, why) in [
            (stall, false, "the body stalled"),
            (deadline, true, "the body took too long"),
        ] {
            let running = run_tuned(&options(root.path(), Duration::from_millis(200)), tuning);
            let stream = sent(&running, head.as_bytes());
            let mut writer = stream.try_clone().expect("clone");
            let trickle = std::thread::spawn(move || {
                while trickled && writer.write_all(b"a").is_ok() {
                    std::thread::sleep(Duration::from_millis(5));
                }
            });
            let answer = read_all(stream);
            assert!(answer.starts_with("HTTP/1.1 408 "), "{why}: {answer}");
            assert!(answer.contains(why), "{answer}");
            trickle.join().expect("trickle");
            until("the PUT slot is let go", || reading(&running) == 0);
            put(&running, &envelope(&OLD, 1)).expect("the slot serves the next PUT");
            running.stop();
        }
    }

    #[test]
    fn a_join_asked_again_keeps_its_place_moves_its_time_hourly_and_a_new_proof_replaces_it() {
        use std::os::unix::fs::MetadataExt;
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run_tuned(
            &options(root.path(), Duration::from_millis(200)),
            changeable(),
        );
        put(&running, &envelope(&OLD, 1)).expect("first");
        let proof = |n: u8| crate::b64::encode_padded(&[n; 32]);
        let line = format!("{} proof={}", join_line(1, "m22"), proof(1));
        transport::post_join(&url(&running), &line).expect("queued");
        transport::post_join(&url(&running), &join_line(2, "m2")).expect("one behind it");
        let joins = root.path().join("alab.joins");
        let inode = |path: &Path| std::fs::metadata(path).expect("joins").ino();
        let first = inode(&joins);
        let queued_at = |time: u64| {
            running
                .shared
                .lock_server_state()
                .joins
                .get_mut("alab")
                .expect("queue")[0]
                .request
                .time = Some(time);
        };
        let listed = || transport::list_joins(&url(&running)).expect("listed");
        let hosts = |listed: &[String]| {
            listed
                .iter()
                .map(|line| parse_join_line(line).expect("parses").host)
                .collect::<Vec<_>>()
        };

        let recent = now() - (JOIN_REFRESH_EVERY_SECS - 60);
        queued_at(recent);
        transport::post_join(&url(&running), &line).expect("asked again");
        assert_eq!(
            listed()[0],
            format!("{} {recent} proof={}", join_line(1, "m22"), proof(1))
        );
        assert_eq!(inode(&joins), first, "the store was rewritten");

        let earlier = now() - JOIN_REFRESH_EVERY_SECS;
        queued_at(earlier);
        let set_peer = |peer: Option<IpAddr>| {
            running
                .shared
                .lock_server_state()
                .joins
                .get_mut("alab")
                .expect("queue")[0]
                .peer = peer;
        };
        let here = running.shared.lock_server_state().joins["alab"][0].peer;
        let there: IpAddr = "192.0.2.7".parse().expect("address");
        set_peer(Some(there));
        transport::post_join(&url(&running), &line).expect("asked again, an hour on");
        let now_listed = listed();
        assert_eq!(hosts(&now_listed), ["m22", "m2"]);
        let request = parse_join_line(&now_listed[0]).expect("parses");
        assert_eq!(request.proof, Some(proof(1)));
        assert!(
            request.time.is_some_and(|time| time > earlier),
            "{request:?}"
        );
        assert_ne!(inode(&joins), first, "the moved time was not stored");
        let stored = std::fs::read_to_string(&joins).expect("store");
        assert!(
            stored.starts_with(&format!("peer {there} 1\n{}\n", now_listed[0])),
            "{stored}"
        );
        let moved = inode(&joins);
        set_peer(here);

        transport::post_join(
            &url(&running),
            &format!("{} proof={}", join_line(1, "m22"), proof(2)),
        )
        .expect("a new proof");
        let now_listed = listed();
        assert_eq!(hosts(&now_listed), ["m2", "m22"]);
        let request = parse_join_line(&now_listed[1]).expect("parses");
        assert_eq!(request.proof, Some(proof(2)));
        assert_ne!(inode(&joins), moved);
        transport::post_join(&url(&running), &join_line(1, "m22b")).expect("a new host");
        let now_listed = listed();
        assert_eq!(hosts(&now_listed), ["m2", "m22b"]);
        let request = parse_join_line(&now_listed[1]).expect("parses");
        assert_eq!(request.proof, None);
        running.stop();
    }

    #[test]
    fn another_address_can_neither_replace_nor_hide_a_queued_join_request() {
        use std::sync::atomic::AtomicU64;
        const START: u64 = 1_800_000_000;
        static CLOCK: AtomicU64 = AtomicU64::new(START);
        fn clock() -> u64 {
            CLOCK.load(Ordering::SeqCst)
        }
        let gap = JOIN_CHANGE_GAP.as_secs();
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run_tuned(
            &options(root.path(), Duration::from_millis(200)),
            Tuning {
                now: clock,
                ..Tuning::default()
            },
        );
        put(&running, &envelope(&OLD, 1)).expect("first");
        let machine: IpAddr = "192.0.2.10".parse().expect("address");
        let stranger: IpAddr = "192.0.2.66".parse().expect("address");
        let ask = |line: &str, from: IpAddr| {
            post_join(&running.shared, "alab", line.as_bytes(), from).status
        };
        let proof = |n: u8| crate::b64::encode_padded(&[n; 32]);
        let key = crate::transport::test_recipient(1);
        let asked = format!("{} proof={}", join_line(1, "m30"), proof(1));
        assert_eq!(ask(&asked, machine), 204);

        assert_eq!(ask(&format!("{key} m30 {}", code(2)), stranger), 400);
        CLOCK.store(START + gap, Ordering::SeqCst);
        assert_eq!(
            ask(
                &format!("{} proof={}", join_line(1, "evil"), proof(2)),
                stranger
            ),
            204
        );
        let listed = transport::list_joins(&url(&running)).expect("listed");
        assert_eq!(listed.len(), 2, "{listed:?}");
        assert_eq!(
            listed[0],
            format!("{} {START} proof={}", join_line(1, "m30"), proof(1)),
            "the machine's request stands as it asked it"
        );

        CLOCK.store(START + 2 * gap, Ordering::SeqCst);
        assert_eq!(
            ask(
                &format!("{} proof={}", join_line(1, "evil"), proof(3)),
                stranger
            ),
            204
        );
        assert_eq!(
            ask(
                &format!("{} proof={}", join_line(1, "m30"), proof(4)),
                machine
            ),
            204
        );
        let moved: IpAddr = "192.0.2.11".parse().expect("address");
        assert_eq!(
            ask(
                &format!("{} proof={}", join_line(1, "m30"), proof(4)),
                moved
            ),
            204
        );
        let state = running.shared.lock_server_state();
        let mine: Vec<&Queued> = state.joins["alab"]
            .iter()
            .filter(|queued| queued.request.host == "m30")
            .collect();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].peer, Some(machine));
        assert_eq!(mine[0].request.proof, Some(proof(4)));
        drop(state);

        let hour = START + 2 * gap + JOIN_REFRESH_EVERY_SECS;
        CLOCK.store(hour, Ordering::SeqCst);
        let asked = format!("{} proof={}", join_line(1, "m30"), proof(4));
        assert_eq!(ask(&asked, stranger), 204);
        CLOCK.store(hour + 1, Ordering::SeqCst);
        let asked = format!("{} proof={}", join_line(1, "m30"), proof(5));
        assert_eq!(ask(&asked, machine), 204);

        let mut at = hour + 1;
        while at < hour + 1 + JOIN_TTL_SECS + 2 * JOIN_REFRESH_EVERY_SECS {
            at += JOIN_REFRESH_EVERY_SECS;
            CLOCK.store(at, Ordering::SeqCst);
            assert_eq!(ask(&asked, moved), 204);
            let listed = transport::list_joins(&url(&running)).expect("listed");
            let refreshed = format!("{} {at} proof={}", join_line(1, "m30"), proof(5));
            assert!(listed.contains(&refreshed), "{listed:?}");
        }
        let state = running.shared.lock_server_state();
        assert_eq!(state.joins["alab"].len(), 1);
        assert_eq!(state.joins["alab"][0].peer, Some(machine));
        drop(state);
        running.stop();
    }

    #[test]
    fn a_key_two_addresses_ask_for_is_let_in_once() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(
            &root.path().join("store"),
            Duration::from_millis(200),
        ));
        let vault = format!("http://{}/v1/twice", running.addr);
        let admin = crate::client::Paths::for_home(&root.path().join("ma"));
        crate::client::create(&admin, &vault, None, Some("ma")).expect("create");
        let opened = crate::client::admin_open(&admin).expect("open");
        let mut doc = opened.doc.clone();
        doc.set_open(true).expect("open");
        crate::client::admin_publish(&admin, &opened, &doc).expect("publish");
        let key = crate::transport::test_recipient(1);
        for (host, from) in [("mb", "192.0.2.10"), ("evil", "192.0.2.66")] {
            let line = format!("{key} {host} {}", code(1));
            let from = from.parse().expect("address");
            assert_eq!(
                post_join(&running.shared, "twice", line.as_bytes(), from).status,
                204
            );
        }
        let done = crate::client::auto_approve(&admin).expect("approve");
        assert_eq!(done.approved, ["mb"]);
        let doc = crate::client::admin_open(&admin).expect("open").doc.clone();
        let names: Vec<String> = doc
            .machines()
            .into_iter()
            .filter(|machine| machine.recipient == key)
            .map(|machine| machine.name)
            .collect();
        assert_eq!(names, ["mb"]);
        running.stop();
    }

    #[test]
    fn one_address_queues_at_most_4_joins_and_the_listing_never_names_it() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let joins = root.path().join("alab.joins");
        let running = run_tuned(
            &options(root.path(), Duration::from_millis(200)),
            changeable(),
        );
        put(&running, &envelope(&OLD, 1)).expect("first");
        for n in 1..=4u8 {
            transport::post_join(&url(&running), &join_line(n, &format!("m{n}"))).expect("queued");
        }
        let error = transport::post_join(&url(&running), &join_line(5, "m5")).expect_err("a fifth");
        assert!(error.0.contains("429"), "{error}");
        assert!(error.0.contains("your address"), "{error}");
        transport::post_join(&url(&running), &join_line(4, "m4b")).expect("replaced");

        let listed = transport::list_joins(&url(&running)).expect("listed");
        assert_eq!(listed.len(), 4, "{listed:?}");
        for line in &listed {
            assert_eq!(parse_join_line(line).expect("parses").listed(), *line);
            assert!(!line.contains("127.0.0.1"), "{line}");
        }
        let stored = std::fs::read_to_string(&joins).expect("joins");
        assert_eq!(stored.matches("peer 127.0.0.1 1\n").count(), 4, "{stored}");
        running.stop();

        let running = run(&options(root.path(), Duration::from_millis(200)));
        let error =
            transport::post_join(&url(&running), &join_line(5, "m5")).expect_err("still a fifth");
        assert!(error.0.contains("429"), "{error}");
        running.stop();

        let old: String = stored
            .lines()
            .filter(|line| !line.starts_with("peer "))
            .map(|line| format!("{line}\n"))
            .collect();
        fsx::write_atomic(&joins, old.as_bytes(), 0o600).expect("write");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        assert_eq!(
            transport::list_joins(&url(&running)).expect("listed").len(),
            4
        );
        transport::post_join(&url(&running), &join_line(5, "m5")).expect("counted against no one");
        running.stop();
    }

    #[test]
    fn a_publish_answers_the_queued_joins_and_one_still_waiting_counts_when_it_asks_again() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        put(&running, &envelope(&OLD, 1)).expect("first");
        let ask = |n: u8| transport::post_join(&url(&running), &join_line(n, &format!("m{n}")));
        for n in 1..=4 {
            ask(n).expect("queued");
        }
        assert!(ask(5).expect_err("a fifth").0.contains("429"));

        put(&running, &envelope(&OLD, 2)).expect("a new version");
        ask(5).expect("m1 gave way");
        ask(3).expect("asked again");
        ask(4).expect("asked again");
        ask(6).expect("m2 gave way");
        let error = ask(7).expect_err("a fifth counted");
        assert!(error.0.contains("your address"), "{error}");
        let hosts: Vec<String> = transport::list_joins(&url(&running))
            .expect("listed")
            .iter()
            .map(|line| parse_join_line(line).expect("parses").host)
            .collect();
        assert_eq!(hosts, ["m3", "m4", "m5", "m6"]);
        running.stop();
    }

    #[test]
    fn a_key_changes_its_request_at_most_every_10_s_and_the_log_notes_it_hourly() {
        use std::sync::atomic::AtomicU64;
        const START: u64 = 1_800_000_000;
        static CLOCK: AtomicU64 = AtomicU64::new(START);
        fn clock() -> u64 {
            CLOCK.load(Ordering::SeqCst)
        }
        let at = |secs: u64| CLOCK.store(START + secs, Ordering::SeqCst);
        let gap = JOIN_CHANGE_GAP.as_secs();
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let joins = root.path().join("alab.joins");
        let running = run_tuned(
            &options(root.path(), Duration::from_millis(200)),
            Tuning {
                now: clock,
                ..Tuning::default()
            },
        );
        put(&running, &envelope(&OLD, 1)).expect("first");
        transport::post_join(&url(&running), &join_line(1, "m22")).expect("queued");
        let first = std::fs::read(&joins).expect("joins");
        let held = |running: &Running| running.shared.lock_server_state().joins["alab"][0].clone();
        assert_eq!(
            held(&running).noted,
            START,
            "a new request is noted as it is queued"
        );
        assert_eq!(held(&running).request.time, Some(START));

        at(gap - 1);
        let error = transport::post_join(&url(&running), &join_line(1, "m22b"))
            .expect_err("changed too soon");
        assert!(error.0.contains("429"), "{error}");
        assert!(error.0.contains("changed less than 10s ago"), "{error}");
        assert_eq!(std::fs::read(&joins).expect("joins"), first);
        assert_eq!(held(&running).request.host, "m22");
        assert_eq!(held(&running).request.time, Some(START));

        at(gap);
        transport::post_join(&url(&running), &join_line(1, "m22b")).expect("changed");
        assert_eq!(held(&running).request.host, "m22b");
        assert_eq!(held(&running).request.time, Some(START + gap));
        assert_eq!(held(&running).noted, START, "noted again within the hour");
        assert_ne!(std::fs::read(&joins).expect("joins"), first);
        at(JOIN_NOTE_EVERY_SECS - gap);
        transport::post_join(&url(&running), &join_line(1, "m22c")).expect("changed");
        assert_eq!(held(&running).request.host, "m22c");
        assert_eq!(held(&running).noted, START, "noted again within the hour");
        at(JOIN_NOTE_EVERY_SECS);
        transport::post_join(&url(&running), &join_line(1, "m22d")).expect("changed");
        assert_eq!(held(&running).request.host, "m22d");
        assert_eq!(
            held(&running).noted,
            START + JOIN_NOTE_EVERY_SECS,
            "an hour on, the change is noted"
        );
        running.stop();
    }

    #[test]
    fn a_clock_stepped_back_holds_off_no_change_and_drops_no_request() {
        use std::sync::atomic::AtomicU64;
        const START: u64 = 1_800_000_000;
        static CLOCK: AtomicU64 = AtomicU64::new(START);
        fn clock() -> u64 {
            CLOCK.load(Ordering::SeqCst)
        }
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run_tuned(
            &options(root.path(), Duration::from_millis(200)),
            Tuning {
                now: clock,
                ..Tuning::default()
            },
        );
        put(&running, &envelope(&OLD, 1)).expect("first");
        transport::post_join(&url(&running), &join_line(1, "m22")).expect("queued");
        transport::post_join(&url(&running), &join_line(2, "m23")).expect("queued");

        let back = START - 6 * 60 * 60;
        CLOCK.store(back, Ordering::SeqCst);
        transport::post_join(&url(&running), &join_line(1, "m22b")).expect("changed at once");
        let held = |running: &Running| running.shared.lock_server_state().joins["alab"].clone();
        let hosts: Vec<(String, Option<u64>)> = held(&running)
            .into_iter()
            .map(|queued| (queued.request.host, queued.request.time))
            .collect();
        assert_eq!(
            hosts,
            [
                ("m23".to_owned(), Some(START)),
                ("m22b".to_owned(), Some(back))
            ]
        );

        CLOCK.store(back - 2 * 86_400, Ordering::SeqCst);
        assert_eq!(
            transport::list_joins(&url(&running)).expect("listed").len(),
            2
        );
        running.stop();
    }

    #[test]
    fn a_join_waiting_on_its_store_write_holds_up_no_read_or_publish() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::from_millis(200)));
        put(&running, &envelope(&OLD, 1)).expect("first");
        let writing = running.shared.join_writes();
        let target = url(&running);
        let poster =
            std::thread::spawn(move || transport::post_join(&target, &join_line(1, "m22")));
        assert_eq!(
            get(&running).expect("served"),
            Fetched::Body(envelope(&OLD, 1))
        );
        put(&running, &envelope(&OLD, 2)).expect("published");
        assert!(
            transport::list_joins(&url(&running))
                .expect("listed")
                .is_empty()
        );
        drop(writing);
        poster.join().expect("poster").expect("queued");
        assert_eq!(
            transport::list_joins(&url(&running)).expect("listed").len(),
            1
        );
        running.stop();
    }

    #[test]
    fn the_join_store_keeps_each_address_where_the_listing_never_shows_it() {
        let request = |n: u8| JoinRequest {
            time: Some(1_792_530_000),
            ..parse_join_line(&join_line(n, &format!("m{n}"))).expect("valid")
        };
        let here: IpAddr = "192.0.2.7".parse().expect("address");
        let there: IpAddr = "2001:db8::7".parse().expect("address");
        let queue = vec![
            queued(request(1), Some(here)),
            queued(request(2), None),
            queued(request(3), Some(there)),
        ];
        let [one, two, three] = [1, 2, 3].map(|n| request(n).listed());
        let text = stored_lines(&queue);
        assert_eq!(
            text,
            format!("peer 192.0.2.7 1\n{one}\n{two}\npeer 2001:db8::7 1\n{three}\n")
        );
        assert_eq!(join_lines(&queue), format!("{one}\n{two}\n{three}\n"));
        let triples = |queue: &[Queued]| {
            queue
                .iter()
                .map(|queued| (queued.request.clone(), queued.peer, queued.asked))
                .collect::<Vec<_>>()
        };
        let read = parse_stored(&text);
        assert_eq!(triples(&read[..1]), triples(&queue[..1]));
        assert_eq!(triples(&read[2..]), triples(&queue[2..]));
        assert_eq!(
            (read[1].request.clone(), read[1].peer, read[1].asked),
            (request(2), None, 0)
        );
        let odd = format!(
            "peer 192.0.2.7\nnot a request\n{one}\npeer nonsense 3\n{two}\npeer 192.0.2.7\n{three}\n"
        );
        let read = parse_stored(&odd);
        assert_eq!(read.len(), 3);
        assert!(read[..2].iter().all(|queued| queued.peer.is_none()));
        assert_eq!((read[2].peer, read[2].asked), (Some(here), 0));
    }

    #[test]
    fn a_first_version_is_taken_only_from_this_machine_or_under_a_pin() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(
            &root.path().join("here"),
            Duration::from_millis(200),
        ));
        put(&running, &envelope(&OLD, 1)).expect("from loopback");
        running.stop();

        let elsewhere = Tuning {
            is_local: |_, _| false,
            ..Tuning::default()
        };
        let store = root.path().join("there");
        let running = run_tuned(&options(&store, Duration::from_millis(200)), elsewhere);
        let error = put(&running, &envelope(&OLD, 1)).expect_err("from another machine");
        assert!(error.0.contains("403"), "{error}");
        assert!(
            error.0.contains(
                "only from the server's own Mac, or under aterm secrets serve --pin <vault>=<key>"
            ),
            "{error}"
        );
        assert!(stored(&store).is_none());
        let answer = raw(
            &running,
            b"PUT /v1/alab HTTP/1.1\r\nHost: x\r\nContent-Length: 1000\r\n\r\n",
        );
        assert!(answer.starts_with("HTTP/1.1 403 "), "{answer}");
        assert_eq!(reading(&running), 0);
        running.stop();

        let mut pinned = options(&store, Duration::from_millis(200));
        pinned.pins = vec![("alab".to_owned(), signer_public(&OLD).expect("public"))];
        let running = run_tuned(&pinned, elsewhere);
        put(&running, &envelope(&OLD, 1)).expect("under the pin");
        put(&running, &envelope(&OLD, 2)).expect("a later version");
        let other = Source::Url(format!("http://{}/v1/other", running.addr));
        let error = transport::publish(&other, &envelope_with(&OLD, 1, Vec::new(), "other"))
            .expect_err("unpinned");
        assert!(error.0.contains("403"), "{error}");
        running.stop();
    }

    #[test]
    fn this_machine_is_loopback_and_its_own_addresses() {
        let ip = |text: &str| text.parse::<IpAddr>().expect("address");
        for local in ["127.0.0.1", "127.0.0.9", "::1", "::ffff:127.0.0.1"] {
            assert!(this_machine(ip(local), None), "{local}");
        }
        assert!(this_machine(ip("192.0.2.55"), Some(ip("192.0.2.55"))));
        assert!(this_machine(
            ip("::ffff:192.0.2.55"),
            Some(ip("192.0.2.55"))
        ));
        assert!(!this_machine(ip("192.0.2.55"), Some(ip("127.0.0.1"))));
        assert!(!this_machine(ip("192.0.2.55"), None));
        if let Some(primary) = primary_ipv4() {
            assert!(this_machine(IpAddr::V4(primary), None));
        }
        if let Some(primary) = primary_ipv6() {
            assert!(this_machine(IpAddr::V6(primary), None));
        }
    }

    #[test]
    fn a_hold_too_long_to_add_ends_with_a_newer_version() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), Duration::MAX));
        put(&running, &envelope(&OLD, 1)).expect("first");
        let target = source(&running);
        let waiter = std::thread::spawn(move || transport::fetch(&target, Some(1), HANG_DETECTOR));
        until_waiting(&running, 1);
        put(&running, &envelope(&OLD, 2)).expect("newer");
        assert_eq!(
            waiter.join().expect("waiter").expect("woken"),
            Fetched::Body(envelope(&OLD, 2))
        );
        running.stop();
    }

    #[test]
    fn a_replaced_program_stops_the_server_and_ends_it_cleanly() {
        assert!(program_stamp().is_some());
        assert_eq!(program_stamp(), program_stamp());
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), 2 * HANG_DETECTOR));
        put(&running, &envelope(&OLD, 1)).expect("first");
        let target = source(&running);
        let waiter = std::thread::spawn(move || transport::fetch(&target, Some(1), HANG_DETECTOR));
        until_waiting(&running, 1);
        let shared = Arc::clone(&running.shared);
        let looks = AtomicUsize::new(0);
        let stamp = || {
            let look = looks.fetch_add(1, Ordering::SeqCst);
            Some((1, if look < 3 { 1 } else { 2 }, 0, 0))
        };
        supervise(
            running,
            Duration::from_millis(1),
            &stamp,
            &AtomicBool::new(false),
        )
        .expect("a clean exit");
        assert!(looks.load(Ordering::SeqCst) >= 4);
        assert!(shared.stopping.load(Ordering::SeqCst));
        assert_eq!(
            waiter.join().expect("waiter").expect("ended"),
            Fetched::NoNewer(Some(1))
        );
        until("every connection has closed", || {
            shared.active.load(Ordering::SeqCst) == 0
        });
    }

    #[test]
    fn a_raised_stop_flag_stops_the_server_and_ends_it_cleanly() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = run(&options(root.path(), 2 * HANG_DETECTOR));
        put(&running, &envelope(&OLD, 1)).expect("first");
        let target = source(&running);
        let waiter = std::thread::spawn(move || transport::fetch(&target, Some(1), HANG_DETECTOR));
        until_waiting(&running, 1);
        let shared = Arc::clone(&running.shared);
        let signalled = Arc::new(AtomicBool::new(false));
        let supervisor = {
            let signalled = Arc::clone(&signalled);
            std::thread::spawn(move || supervise(running, 2 * HANG_DETECTOR, &|| None, &signalled))
        };
        signalled.store(true, Ordering::SeqCst);
        supervisor.thread().unpark();
        until("the supervisor returns", || supervisor.is_finished());
        supervisor
            .join()
            .expect("supervisor")
            .expect("a clean exit");
        assert!(shared.stopping.load(Ordering::SeqCst));
        assert_eq!(
            waiter.join().expect("waiter").expect("ended"),
            Fetched::NoNewer(Some(1))
        );
    }

    #[test]
    fn letting_the_servers_children_go_ends_them() {
        let sleep = aterm_uds::deadline::stand_in(Path::new("/bin/sleep"));
        let sleep = sleep.to_str().expect("a UTF-8 path");
        let children = Children::default();
        let pids = {
            let mut set = children.lock();
            let caffeinate = quiet(sleep, &["60".to_owned()]).expect("a stand-in");
            let dns_sd = quiet(sleep, &["60".to_owned()]).expect("a stand-in");
            let pids = [caffeinate.id(), dns_sd.id()];
            set.caffeinate = Some(caffeinate);
            set.dns_sd = Some((dns_sd, Ipv4Addr::LOCALHOST));
            pids
        };
        assert!(pids.iter().all(|&pid| aterm_uds::process::pid_alive(pid)));
        drop(children);
        for pid in pids {
            assert!(
                !aterm_uds::process::pid_alive(pid),
                "{pid} outlived the server"
            );
        }
    }

    #[test]
    fn a_client_that_never_finishes_its_head_is_cut_at_the_deadline() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let tuning = Tuning {
            head_deadline: Duration::from_millis(100),
            linger: Duration::from_millis(100),
            ..Tuning::default()
        };
        let running = run_tuned(&options(root.path(), Duration::from_millis(200)), tuning);
        for start in [&b""[..], b"GET /v1/alab HTTP/1.1\r\nHost: x\r\n"] {
            let answer = read_all(sent(&running, start));
            assert!(answer.starts_with("HTTP/1.1 408 "), "{answer}");
        }
        let stream = sent(&running, b"GET /v1/alab HTTP/1.1\r\nX-Slow: ");
        let mut writer = stream.try_clone().expect("clone");
        let trickle = std::thread::spawn(move || {
            while writer.write_all(b"a").is_ok() {
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let answer = read_all(stream);
        assert!(answer.starts_with("HTTP/1.1 408 "), "{answer}");
        trickle.join().expect("trickle");
        until("the cut connections are closed", || {
            running.shared.active.load(Ordering::SeqCst) == 0
        });
        running.stop();
    }

    #[test]
    fn an_answer_read_too_slowly_is_cut_at_its_deadline() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let tuning = Tuning {
            answer_deadline: Duration::from_millis(200),
            write_stall: 2 * HANG_DETECTOR,
            ..Tuning::default()
        };
        let running = run_tuned(&options(root.path(), Duration::from_millis(200)), tuning);
        let big = Arc::new(vec![b'x'; 15 << 20]);
        running.shared.lock_server_state().vaults.insert(
            "alab".to_owned(),
            Slot {
                held: Held {
                    seq: 1,
                    signer: OLD,
                    handoff: Vec::new(),
                    bytes: Arc::clone(&big),
                },
                wake: Arc::new(Condvar::new()),
            },
        );
        let stream = sent(&running, b"GET /v1/alab HTTP/1.1\r\nHost: x\r\n\r\n");
        until("the server gives up on the answer", || {
            running.shared.served.load(Ordering::SeqCst) == 1
                && running.shared.active.load(Ordering::SeqCst) == 0
        });
        let answer = read_all(stream);
        assert!(
            answer.starts_with("HTTP/1.1 200 "),
            "{}",
            &answer[..answer.len().min(200)]
        );
        assert!(
            answer.len() < big.len(),
            "the whole answer was sent: the socket buffers held it"
        );
        running.stop();
    }
}
