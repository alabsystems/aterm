// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The keeper's loop: [`crate::core::KeeperCore`] over a real socket, real descriptors and the
//! kernel.
//!
//! One thread and one `poll(2)` over the listener, every connection and every exit watch, so no
//! descriptor can leak through a concurrent spawn (the Darwin race `aterm-uds/src/lifeline.rs`
//! records). The loop owns every custody copy (`custody`, keyed by rdev) and does exactly what
//! the core decides with them: keep, close, or send a duplicate in an OFFER. It never reads,
//! writes, ioctls or changes the file-status flags of a master (those flags are shared by every
//! duplicate of the description); `fstat` for the rdev is the one call it makes on one. It
//! signals nothing. The only program it starts is the relaunch, `/usr/bin/open -a <its own
//! bundle>`, inheriting nothing (`aterm_uds::pspawn`).
//!
//! The crash-restore companions (`companions`, keyed by the number the loop gives each) are held
//! the same way: never mapped, read or written (guard `keeper-no-mmap`). Before the core sees
//! one, the loop checks what the descriptor is through libproc and `fcntl` alone
//! ([`companion_fault`]): POSIX shared memory of the claimed length, opened read-only, within
//! the bounds. A companion closes with its master, whatever closes it.

use std::collections::BTreeMap;
use std::io;
use std::os::fd::{AsFd as _, AsRawFd as _, OwnedFd};
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::CtlStream;
use aterm_uds::exitwatch::ExitWatch;

use crate::core::{CompanionId, ConnId, Holders, KeeperCore, KeeperEnv, MarkerEvidence, Out, Rdev};
use crate::frameio::{Incoming, read_frame, write_frame};
use crate::identity::{CheckError, Identity};
use crate::sys::{self, POLLERR, POLLHUP, POLLIN, PollFd};
use crate::wire::{
    Birth, CAP_JOURNAL, CAP_SYNC, COMPANION_JOURNAL, Frame, JournalOffer, MarkerRef, MasterHeader,
    PROTO, RefuseCode,
};

/// How a relaunch is carried out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Relauncher {
    /// Never: this keeper runs outside an app bundle.
    Unavailable,
    /// `/usr/bin/open -a <bundle>`: the only production form.
    OpenBundle(PathBuf),
    /// A test seam, reachable from the library only (no flag spells it): run this argv instead,
    /// so a test can count relaunches without launching an app.
    TestCommand(Vec<String>),
}

/// What `serve` is told.
#[derive(Debug)]
pub struct ServeConfig {
    /// The socket to bind. Its directory must exist, be ours and be private.
    pub socket: PathBuf,
    /// Who may connect.
    pub identity: Identity,
    /// How to relaunch.
    pub relaunch: Relauncher,
    /// The keeper's version string (WELCOME, status).
    pub version: String,
    /// A test seam, library-only: close every registered master at once instead of keeping it,
    /// the negative control that shows the custody copy keeps a shell alive.
    pub test_drop_custody: bool,
}

struct Conn {
    stream: CtlStream,
    pid: u32,
    /// A frame to this peer could not be written ([`Server::send`]): nothing
    /// more is written to it, and its frames are still read through its EOF.
    unwritable: bool,
}

/// The server. [`Server::run`] is the keeper; [`Server::step`] is one turn of
/// it, for tests that drive it in-process.
pub struct Server {
    listener: UnixListener,
    socket: PathBuf,
    core: KeeperCore,
    conns: BTreeMap<ConnId, Conn>,
    custody: BTreeMap<Rdev, OwnedFd>,
    /// Every crash-restore companion held, by the number given at its arrival, with the master
    /// it rides beside.
    companions: BTreeMap<CompanionId, (Rdev, OwnedFd)>,
    next_companion: CompanionId,
    watches: BTreeMap<u32, ExitWatch>,
    children: Vec<u32>,
    identity: Identity,
    relaunch: Relauncher,
    version: String,
    test_drop_custody: bool,
    next_conn: ConnId,
    started: Instant,
    own_pid: u32,
    refused_dials: u64,
    /// COMPANIONs this keeper accepted since it started (`status companions_taken=`): each one
    /// a region it now holds in place of whatever its claim held before. A window's memory
    /// shed replaces a session's region, so the count moving is how a reader of the status
    /// sees that the keeper holds the new region rather than the one it superseded (the
    /// count of regions held, `journals=`, stays the same).
    companions_taken: u64,
    /// Why the last dial was refused unchecked, as last printed: printed once per reason, and
    /// forgotten at the next admitted dial.
    unchecked_said: Option<String>,
    /// Frames read after their writer's exit had already arrived, before the
    /// exit was recorded ([`Server::drain_pid`]).
    frames_behind_exit: u64,
    /// Frames the keeper could not write to their peer ([`Server::send`]).
    replies_lost: u64,
    /// A test seam (unit tests only): the next OFFER's duplicate fails as `EMFILE` would.
    #[cfg(test)]
    test_fail_next_dup: bool,
    /// A test seam: every reason a dial was turned away unchecked, never cleared
    /// ([`Server::test_unchecked_ever`]).
    #[cfg(test)]
    test_unchecked_ever: Vec<String>,
}

/// A per-connection receive/send budget: once a frame's header has arrived,
/// the rest must follow within this, or the connection is dropped; a frame the
/// peer does not take within it is the last one written to it
/// ([`Server::send`]). One slow peer cannot stall the loop.
const IO_TIMEOUT: Duration = Duration::from_secs(2);
/// The loop's clock tick when nothing is readable.
const TICK_MS: i32 = 250;
/// The most frames [`Server::drain_pid`] reads from one connection of a dead
/// peer: more than a socket buffer of the smallest frame (8 KiB of 8-byte
/// headers).
const DRAIN_MAX_FRAMES: usize = 1024;

struct Env<'a> {
    own_pid: u32,
    peer_pids: Vec<u32>,
    _p: std::marker::PhantomData<&'a ()>,
}

impl Env<'_> {
    /// What one device's scan means to the core.
    fn verdict(&self, scan: aterm_uds::holders::HolderScan) -> Holders {
        if !scan.holders.is_empty() {
            Holders::Held(scan.holders)
        // A peer of ours whose table could not be read may hold it. A peer
        // that has died is not one of them, judged or not: the scan reads an
        // exited process as holding nothing (`ESRCH`: the kernel closed its
        // descriptors at the exit).
        } else if scan.unreadable.iter().any(|p| self.peer_pids.contains(p)) {
            Holders::Unknown
        } else {
            Holders::None
        }
    }
}

impl KeeperEnv for Env<'_> {
    fn holders(&mut self, rdev: Rdev) -> Holders {
        self.holders_of(&[rdev]).pop().unwrap_or(Holders::Unknown)
    }
    /// One walk of the uid's processes for the whole set, none for an empty one.
    fn holders_of(&mut self, rdevs: &[Rdev]) -> Vec<Holders> {
        match aterm_uds::holders::scan_holders_of(rdevs, &[self.own_pid]) {
            Ok(scans) => scans.into_iter().map(|scan| self.verdict(scan)).collect(),
            Err(_) => rdevs.iter().map(|_| Holders::Unknown).collect(),
        }
    }
    fn shell_alive(&mut self, pid: u32, birth: Birth) -> bool {
        born(pid) == Some(birth)
    }
    fn pid_alive(&mut self, pid: u32, birth: Option<Birth>) -> bool {
        match birth {
            Some(b) => born(pid) == Some(b),
            None => aterm_uds::process::pid_alive(pid),
        }
    }
    fn marker(&mut self, pid: u32, marker: &MarkerRef) -> MarkerEvidence {
        read_marker(pid, marker)
    }
}

/// What the crash marker of window `pid` says now (§5.4 row 3). The keeper builds the file names
/// itself from the kernel's pid for the connection and the start the HELLO named: both
/// `crash-marker-<pid>-<nanos>-<app|other>.log` (an update's candidate renames `-other` to `-app`
/// at its Commit) and the `.seen` name a later start gives a dead marker it read. So a window can
/// point the keeper at nothing but its own marker. Only an absolute directory of ours counts: a
/// marker missing from a directory the keeper cannot see is no evidence. Nothing is written, and
/// a found marker is opened read-only, never following a symlink, only to try its lock.
#[must_use]
pub fn read_marker(pid: u32, marker: &MarkerRef) -> MarkerEvidence {
    use std::os::unix::ffi::OsStrExt as _;
    let dir = Path::new(std::ffi::OsStr::from_bytes(&marker.dir));
    if !dir.is_absolute() {
        return MarkerEvidence::Unknown;
    }
    match std::fs::symlink_metadata(dir) {
        Ok(m) if m.file_type().is_dir() && m.uid() == aterm_uds::peer::our_uid() => {}
        _ => return MarkerEvidence::Unknown,
    }
    for owner in ["app", "other"] {
        let name = format!("crash-marker-{pid}-{}-{owner}.log", marker.nanos);
        for (file, seen) in [(name.clone(), false), (format!("{name}.seen"), true)] {
            let path = dir.join(file);
            match std::fs::symlink_metadata(&path) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Ok(m) if m.file_type().is_file() => {
                    if seen {
                        // A later start read it as a death with no exit path.
                        return MarkerEvidence::Dead;
                    }
                    return match aterm_uds::ownerlock::probe(&path) {
                        aterm_uds::ownerlock::Owner::Live => MarkerEvidence::Held,
                        aterm_uds::ownerlock::Owner::Dead => MarkerEvidence::Dead,
                        aterm_uds::ownerlock::Owner::Unknown => MarkerEvidence::Unknown,
                    };
                }
                // A symlink, a directory, an unreadable entry: no evidence.
                _ => return MarkerEvidence::Unknown,
            }
        }
    }
    MarkerEvidence::Released
}

/// Why a descriptor a window sent as a COMPANION is not one the keeper may hold, or `None` when
/// it may (the crash-restore plan §5, frame 14). It must be a crash-restore journal (`kind`),
/// POSIX shared memory (`aterm_uds::holders::fd_is_pshm`: a file, a pipe or a device parked
/// beside a master would otherwise be handed to the next window as a region to map) of exactly
/// the length claimed, a whole number of 16 KiB pages up to 64 MiB (the restorer's own bounds,
/// `aterm_uds::shm`), and open READ-ONLY: a received writable descriptor maps writable, so a
/// window offered it could corrupt the dead window's words (§3.2 N8). Nothing here maps or reads
/// the region.
#[must_use]
pub fn companion_fault(fd: std::os::fd::BorrowedFd<'_>, kind: u8, len: u64) -> Option<String> {
    if kind != COMPANION_JOURNAL {
        return Some(format!("kind {kind} is no journal region"));
    }
    let (page, max) = (aterm_uds::shm::PAGE as u64, aterm_uds::shm::MAX_LEN as u64);
    if len == 0 || !len.is_multiple_of(page) || len > max {
        return Some(format!(
            "a {len}-byte region is not a whole number of {page}-byte pages up to {max}"
        ));
    }
    match aterm_uds::holders::fd_is_pshm(fd) {
        None => return Some("the descriptor is not POSIX shared memory".to_string()),
        Some(size) if size != len => {
            return Some(format!("the region is {size} bytes, not the {len} claimed"));
        }
        Some(_) => {}
    }
    match aterm_uds::shm::is_read_only(fd) {
        Ok(true) => None,
        Ok(false) => Some(
            "the descriptor is open for writing: a window it is offered to could map it writable"
                .to_string(),
        ),
        Err(e) => Some(format!("the descriptor's access mode is unreadable: {e}")),
    }
}

/// A pid's kernel start time, in the wire's shape.
#[must_use]
pub fn born(pid: u32) -> Option<Birth> {
    let b = aterm_uds::process::read_process_birth(i32::try_from(pid).ok()?)?;
    Some(Birth {
        seconds: b.seconds,
        micros: u32::try_from(b.microseconds).ok()?,
    })
}

impl Server {
    /// Bind the socket. A stale socket file at the path is replaced only when
    /// nothing answers on it.
    ///
    /// # Errors
    /// The directory not being private and ours, a live keeper already on the
    /// socket (`AddrInUse`), or the bind failing.
    pub fn bind(config: ServeConfig) -> io::Result<Self> {
        let dir = config
            .socket
            .parent()
            .ok_or_else(|| io::Error::other("the socket path has no directory"))?;
        ensure_private_dir(dir)?;
        if config.socket.exists() {
            if CtlStream::connect(&config.socket).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("a keeper already answers on {}", config.socket.display()),
                ));
            }
            std::fs::remove_file(&config.socket)?;
        }
        let listener = UnixListener::bind(&config.socket)?;
        prove_socket_is_ours(dir, &config.socket)?;
        listener.set_nonblocking(true)?;
        sys::raise_nofile();
        let can_relaunch = config.relaunch != Relauncher::Unavailable;
        Ok(Self {
            listener,
            socket: config.socket,
            core: KeeperCore::new(can_relaunch),
            conns: BTreeMap::new(),
            custody: BTreeMap::new(),
            companions: BTreeMap::new(),
            next_companion: 1,
            watches: BTreeMap::new(),
            children: Vec::new(),
            identity: config.identity,
            relaunch: config.relaunch,
            version: config.version,
            test_drop_custody: config.test_drop_custody,
            next_conn: 1,
            started: Instant::now(),
            own_pid: std::process::id(),
            refused_dials: 0,
            companions_taken: 0,
            unchecked_said: None,
            frames_behind_exit: 0,
            replies_lost: 0,
            #[cfg(test)]
            test_fail_next_dup: false,
            #[cfg(test)]
            test_unchecked_ever: Vec::new(),
        })
    }

    /// The bound socket.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Serve until the process is ended (launchd's SIGTERM, a kill).
    ///
    /// # Errors
    /// `poll` failing for a reason other than a signal.
    pub fn run(&mut self) -> io::Result<()> {
        loop {
            self.step(TICK_MS)?;
        }
    }

    fn now(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn env(&self) -> Env<'static> {
        Env {
            own_pid: self.own_pid,
            peer_pids: self.conns.values().map(|c| c.pid).collect(),
            _p: std::marker::PhantomData,
        }
    }

    /// One turn: wait up to `timeout_ms` for a connection, a frame or an exit,
    /// handle what arrived, then tick the clock.
    ///
    /// # Errors
    /// `poll` failing for a reason other than a signal.
    pub fn step(&mut self, timeout_ms: i32) -> io::Result<()> {
        let mut fds = vec![PollFd {
            fd: self.listener.as_raw_fd(),
            events: POLLIN,
            revents: 0,
        }];
        let conn_ids: Vec<ConnId> = self.conns.keys().copied().collect();
        for id in &conn_ids {
            fds.push(PollFd {
                fd: self.conns[id].stream.as_raw_fd(),
                events: POLLIN,
                revents: 0,
            });
        }
        let watch_pids: Vec<u32> = self.watches.keys().copied().collect();
        for pid in &watch_pids {
            fds.push(PollFd {
                fd: self.watches[pid].as_raw_fd(),
                events: POLLIN,
                revents: 0,
            });
        }
        match sys::poll(&mut fds, timeout_ms) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
        let ready = |pfd: &PollFd| pfd.revents & (POLLIN | POLLHUP | POLLERR) != 0;
        if ready(&fds[0]) {
            self.accept_all();
        }
        for (i, id) in conn_ids.iter().enumerate() {
            if ready(&fds[1 + i]) {
                self.read_conn(*id);
            }
        }
        for (i, pid) in watch_pids.iter().enumerate() {
            if ready(&fds[1 + conn_ids.len() + i]) {
                self.read_watch(*pid);
            }
        }
        // Exits whose kqueue event is already queued but not yet polled are
        // read on every turn too (cheap: a zero-timeout kevent each).
        for pid in self.watches.keys().copied().collect::<Vec<_>>() {
            self.read_watch(pid);
        }
        let now = self.now();
        let mut env = self.env();
        let outs = self.core.tick(now, &mut env);
        self.apply(outs);
        self.children.retain(|pid| !sys::reap_if_done(*pid));
        Ok(())
    }

    fn accept_all(&mut self) {
        loop {
            let stream = match self.listener.accept() {
                Ok((s, _)) => s,
                Err(_) => return,
            };
            let _ = stream.set_nonblocking(false);
            let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
            let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
            // The code check waits on macOS's trustd, under its hang detector
            // (`identity::PEER_CHECK_HANG`), so a silent trustd stalls the loop
            // that long, once — while it stays silent every later dial is
            // refused at once (`aterm_uds::deadline::TRUST`).
            match self.identity.check(&stream, IO_TIMEOUT) {
                Ok(()) => self.unchecked_said = None,
                Err(CheckError::Refused(_)) => {
                    self.refused_dials += 1;
                    continue;
                }
                Err(CheckError::Unchecked(why) | CheckError::Unanswered(why)) => {
                    self.refused_dials += 1;
                    #[cfg(test)]
                    self.test_unchecked_ever.push(why.clone());
                    if self.unchecked_said.as_deref() != Some(why.as_str()) {
                        eprintln!(
                            "keeper: turned away a connection whose identity could not be \
                             verified: {why}"
                        );
                        self.unchecked_said = Some(why);
                    }
                    continue;
                }
            }
            let Ok(pid) = aterm_uds::fdpass::peer_pid(&stream) else {
                self.refused_dials += 1;
                continue;
            };
            let birth = born(pid);
            let id = self.next_conn;
            self.next_conn += 1;
            if !self.core.accept(id, pid, birth) {
                self.refused_dials += 1;
                continue;
            }
            // The death witness is registered at the connect, while the kernel
            // has just named the pid (§5.3 step 5).
            if !self.watches.contains_key(&pid)
                && let Some(w) = ExitWatch::watch(pid)
            {
                self.watches.insert(pid, w);
            }
            self.conns.insert(
                id,
                Conn {
                    stream,
                    pid,
                    unwritable: false,
                },
            );
        }
    }

    fn read_watch(&mut self, pid: u32) {
        let Some(status) = self.watches.get(&pid).and_then(ExitWatch::exit_status) else {
            return;
        };
        self.watches.remove(&pid);
        self.drain_pid(pid);
        let now = self.now();
        let mut env = self.env();
        let outs = self.core.peer_exit(pid, status, now, &mut env);
        self.apply(outs);
    }

    /// Watch `pid`'s exit (a PENDING successor, an orphan's leader). A pid
    /// already gone cannot be watched: whatever waited on it is judged now.
    fn watch_pid(&mut self, pid: u32) {
        if self.watches.contains_key(&pid) {
            return;
        }
        match ExitWatch::watch(pid) {
            Some(w) => {
                self.watches.insert(pid, w);
            }
            None => {
                let mut env = self.env();
                let outs = self.core.prune_dead_orphans(&mut env);
                self.apply(outs);
            }
        }
    }

    fn drop_conn(&mut self, id: ConnId) {
        self.conns.remove(&id);
        let now = self.now();
        let mut env = self.env();
        let outs = self.core.eof(id, now, &mut env);
        self.core.forget_if_passive(id);
        self.apply(outs);
    }

    /// Stream order (§5.3 step 4, "K reads BYE and then EOF"): every frame a peer wrote before it
    /// died (a claim, its BYE) is read before its death is recorded. The exit watch is a
    /// descriptor of its own, so a turn can read the exit while earlier frames still wait on the
    /// connection (a turn reads one frame per connection, and the every-turn sweep reads exits
    /// `poll` never reported). The core refuses a claim from a peer whose exit it has recorded,
    /// so a claim read after the exit would leave its master orphaned after a quit. A dead peer's
    /// buffer is finite and ends in EOF, and the zero-timeout `poll` never waits;
    /// [`DRAIN_MAX_FRAMES`] only stops a connection some live process still writes on.
    fn drain_pid(&mut self, pid: u32) {
        let ids: Vec<ConnId> = self
            .conns
            .iter()
            .filter(|(_, c)| c.pid == pid)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            for _ in 0..DRAIN_MAX_FRAMES {
                let Some(conn) = self.conns.get(&id) else {
                    break;
                };
                let mut pfd = [PollFd {
                    fd: conn.stream.as_raw_fd(),
                    events: POLLIN,
                    revents: 0,
                }];
                match sys::poll(&mut pfd, 0) {
                    Ok(n) if n > 0 && pfd[0].revents & (POLLIN | POLLHUP | POLLERR) != 0 => {
                        if self.read_conn(id) {
                            self.frames_behind_exit += 1;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    _ => break,
                }
            }
        }
    }

    /// Read one frame of `id` and handle it: `true` when a frame was read,
    /// `false` when the connection is over (EOF, a malformed frame, a stall).
    fn read_conn(&mut self, id: ConnId) -> bool {
        let Some(conn) = self.conns.get(&id) else {
            return false;
        };
        let incoming = match read_frame(&conn.stream) {
            Ok(Some(i)) => i,
            // EOF, a malformed frame, a stalled peer: the connection is over.
            Ok(None) | Err(_) => {
                self.drop_conn(id);
                return false;
            }
        };
        self.handle(id, incoming);
        true
    }

    fn handle(&mut self, id: ConnId, incoming: Incoming) {
        let now = self.now();
        let mut env = self.env();
        let Incoming { frame, fd } = incoming;
        let outs = match frame {
            Frame::Hello {
                class,
                caps,
                marker,
                ..
            } => {
                if let Some(marker) = marker {
                    self.core.name_marker(id, marker, &mut env);
                }
                self.core.hello(id, class, caps, now, &mut env)
            }
            Frame::Register { header, tag } => self.register(id, header, tag, fd),
            Frame::Meta { rdev, meta } => self.core.meta(id, rdev, meta),
            Frame::Release { rdev } => self.core.release(id, rdev),
            Frame::Pending { pid } => {
                let birth = born(pid);
                self.core.pending(id, pid, now, birth)
            }
            Frame::Bye => {
                self.core.bye(id);
                Vec::new()
            }
            Frame::Adopt => self.core.adopt(id, &mut env),
            Frame::Status { .. } => {
                let text = self.status_text();
                self.send(id, &Frame::Status { text }, None);
                Vec::new()
            }
            // Every frame `id` wrote before it has been read and handled: this connection's
            // frames are read in order, one at a time.
            Frame::Sync { seq } => {
                self.send(id, &Frame::Sync { seq }, None);
                Vec::new()
            }
            // Accepted; nothing acts on it yet.
            Frame::RestartWhenSafe => Vec::new(),
            Frame::Companion { rdev, kind, len } => self.companion(id, rdev, kind, len, fd),
            Frame::CompanionRelease { rdev, kind } => self.core.companion_release(id, rdev, kind),
            // Keeper-to-window kinds from a window: a protocol error.
            Frame::Welcome { .. }
            | Frame::Offer { .. }
            | Frame::Orphans { .. }
            | Frame::Refused { .. }
            | Frame::CompanionOffer { .. } => {
                self.drop_conn(id);
                return;
            }
        };
        self.apply(outs);
    }

    /// REGISTER: read the descriptor's rdev (`fstat`, the one call ever made
    /// on a master) and let the core decide.
    fn register(
        &mut self,
        id: ConnId,
        header: MasterHeader,
        tag: Vec<u8>,
        fd: Option<OwnedFd>,
    ) -> Vec<Out> {
        let Some(fd) = fd else {
            return vec![Out::Refused {
                conn: id,
                code: crate::wire::RefuseCode::NoDescriptor,
                rdev: header.rdev,
            }];
        };
        let file = std::fs::File::from(fd);
        let fd_rdev = match file.metadata() {
            Ok(m) if m.file_type().is_char_device() => m.rdev(),
            // Not a character device: no master of any terminal.
            _ => u64::MAX,
        };
        let fd = OwnedFd::from(file);
        let outs = self.core.register(id, header, tag, fd_rdev);
        for out in &outs {
            if *out == Out::Keep(header.rdev) && !self.test_drop_custody {
                self.custody.insert(header.rdev, fd);
                return outs;
            }
        }
        // DropDuplicate, a refusal, or the test seam: the arrived copy closes.
        drop(fd);
        outs
    }

    /// COMPANION: check what the descriptor is ([`companion_fault`], which neither maps nor
    /// reads it), number it, and let the core decide; it is held only when the core keeps it.
    fn companion(
        &mut self,
        id: ConnId,
        rdev: Rdev,
        kind: u8,
        len: u64,
        fd: Option<OwnedFd>,
    ) -> Vec<Out> {
        let refused = |code| {
            vec![Out::Refused {
                conn: id,
                code,
                rdev,
            }]
        };
        let Some(fd) = fd else {
            return refused(RefuseCode::NoDescriptor);
        };
        if let Some(why) = companion_fault(fd.as_fd(), kind, len) {
            // The keeper's log names why (a window's bug, or a peer probing); the descriptor
            // closes here.
            eprintln!("keeper: refused a companion for rdev {rdev:#x}: {why}");
            return refused(RefuseCode::NotAJournal);
        }
        let number = self.next_companion;
        self.next_companion += 1;
        let outs = self.core.companion(id, rdev, kind, len, number);
        if outs.contains(&Out::KeepCompanion(number)) {
            self.companions.insert(number, (rdev, fd));
        }
        outs
    }

    fn apply(&mut self, outs: Vec<Out>) {
        for out in outs {
            match out {
                Out::Keep(_) | Out::DropDuplicate(_) => {}
                Out::KeepCompanion(_) => self.companions_taken += 1,
                Out::Close(rdev) => {
                    self.custody.remove(&rdev);
                    // A companion travels only beside its master: none outlives it.
                    self.companions.retain(|_, (of, _)| *of != rdev);
                }
                Out::CloseCompanion(number) => {
                    self.companions.remove(&number);
                }
                Out::Welcome { conn, offers } => {
                    let frame = Frame::Welcome {
                        proto: PROTO,
                        offers,
                        version: self.version.clone(),
                        // What a window that checks code reads before it
                        // registers a master here (§5.7).
                        identity: Some(self.identity.policy()),
                        // A window sends its companions only to a keeper that says it
                        // takes them, and SYNC only to one that answers it; an older window
                        // reads no further than the policy.
                        caps: Some(CAP_JOURNAL | CAP_SYNC),
                    };
                    self.send(conn, &frame, None);
                }
                Out::Offer {
                    conn,
                    rdev,
                    journal,
                } => self.offer(conn, rdev, journal),
                Out::Orphans { conn, n } => self.send(conn, &Frame::Orphans { n }, None),
                Out::Refused { conn, code, rdev } => {
                    self.send(conn, &Frame::Refused { code, rdev }, None);
                }
                Out::WatchPid(pid) => self.watch_pid(pid),
                Out::Relaunch => self.relaunch_now(),
            }
        }
    }

    /// Send the OFFER of `rdev` to `conn` with a duplicate of the custody copy, and to a window
    /// that journals, with the brake and a COMPANION_OFFER of the region right after it. The
    /// kernel duplicates each description into the recipient; the keeper's copies stay here
    /// (F9). Both duplicates are made before anything is written, so an OFFER that says a region
    /// follows is followed by one; a duplicate that cannot be made (`EMFILE`) ends the writing to
    /// `conn` as a failed write does, and the offer returns to Orphaned at its EOF.
    fn offer(&mut self, conn: ConnId, rdev: Rdev, journal: Option<crate::core::OfferJournal>) {
        let Some(rec) = self.core.records().get(&rdev).cloned() else {
            return;
        };
        let Some(master) = self
            .custody
            .get(&rdev)
            .map(|f| f.as_fd().try_clone_to_owned())
        else {
            return;
        };
        #[cfg(test)]
        let master = if std::mem::take(&mut self.test_fail_next_dup) {
            Err(io::Error::from_raw_os_error(24))
        } else {
            master
        };
        // The region: the core's companion, held here (a companion the loop does not hold is
        // offered as none, never as a frame without its descriptor).
        let region = journal.and_then(|j| j.companion).and_then(|c| {
            self.companions
                .get(&c.id)
                .map(|(_, fd)| (c, fd.as_fd().try_clone_to_owned()))
        });
        let (master, region) = match (master, region) {
            (Ok(master), None) => (master, None),
            (Ok(master), Some((c, Ok(fd)))) => (master, Some((c, fd))),
            (Err(_), _) | (_, Some((_, Err(_)))) => {
                self.end_writing(conn);
                return;
            }
        };
        let frame = Frame::Offer {
            header: rec.header,
            tag: rec.tag,
            meta: rec.meta,
            journal: journal.map(|j| JournalOffer {
                attempts: j.attempts,
                companions: u8::from(region.is_some()),
            }),
        };
        self.send(conn, &frame, Some(&master));
        if let Some((c, fd)) = region {
            let frame = Frame::CompanionOffer {
                rdev,
                kind: c.kind,
                len: c.len,
            };
            self.send(conn, &frame, Some(&fd));
        }
    }

    /// Write one frame to `conn`. A failed write (the peer died or closed its end, or took nothing
    /// for [`IO_TIMEOUT`]) ends the keeper's writing to that connection, never its reading: what
    /// the peer wrote before is still read, in order, through its EOF (§5.3 step 4). A dead peer's
    /// frames are read after its death (`drain_pid`), so the answer to one cannot be written, and
    /// dropping the connection would lose every frame behind it (a claim, the BYE).
    fn send(&mut self, conn: ConnId, frame: &Frame, fd: Option<&OwnedFd>) {
        let Some(c) = self.conns.get(&conn) else {
            return;
        };
        if c.unwritable || write_frame(&c.stream, frame, fd.map(|f| f.as_fd())).is_err() {
            self.end_writing(conn);
        }
    }

    /// One frame for `conn` was not written: a write failed ([`Self::send`]), or an OFFER's
    /// duplicate of the custody copy could not be made (`EMFILE`). Writing to `conn` ends (its
    /// write half is shut, so a live peer reads EOF and reconnects) and reading goes on through
    /// its EOF. An offer not written stays Offered to `conn` until that EOF, then returns to
    /// Orphaned ([`KeeperCore::eof`]).
    fn end_writing(&mut self, conn: ConnId) {
        let Some(c) = self.conns.get_mut(&conn) else {
            return;
        };
        if !c.unwritable {
            c.unwritable = true;
            let _ = c.stream.shutdown(std::net::Shutdown::Write);
        }
        self.replies_lost += 1;
    }

    fn relaunch_now(&mut self) {
        let spawned = match &self.relaunch {
            Relauncher::Unavailable => return,
            Relauncher::OpenBundle(bundle) => {
                let bundle = bundle.to_string_lossy().into_owned();
                aterm_uds::pspawn::spawn_cloexec_default("/usr/bin/open", &["-a", &bundle])
            }
            Relauncher::TestCommand(argv) => match argv.split_first() {
                Some((program, rest)) => {
                    let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
                    aterm_uds::pspawn::spawn_cloexec_default(program, &rest)
                }
                None => return,
            },
        };
        // `open`'s exit status is no evidence of a launch (M8): only a HELLO
        // is, which the core waits for.
        if let Ok(pid) = spawned {
            self.children.push(pid);
        }
    }

    /// The status text: who the keeper is, then the core's lines.
    #[must_use]
    pub fn status_text(&self) -> String {
        let mut lines = vec![
            "keeper=running".to_string(),
            format!("pid={} version={}", self.own_pid, self.version),
            format!("socket={}", self.socket.display()),
            format!("identity={}", self.identity.describe()),
            format!("custody_copies={}", self.custody.len()),
            format!("refused_dials={}", self.refused_dials),
            format!("frames_behind_exit={}", self.frames_behind_exit),
            format!("replies_lost={}", self.replies_lost),
        ];
        // The crash-restore regions held beside the masters, counted from the descriptors this
        // loop really holds, sized by `fstat` (the plan's §5 status), never from the core's
        // bookkeeping: a region a teardown failed to close shows here. The size is the address
        // space the regions name (virtual: only the pages their writers touched are resident,
        // and the keeper maps none of them).
        let (journals, bytes) =
            self.companions
                .values()
                .fold((0usize, 0u64), |(n, total), (_, fd)| {
                    let size = aterm_uds::shm::fd_size(fd.as_fd()).unwrap_or(0);
                    (n + 1, total.saturating_add(size))
                });
        lines.push(format!(
            "journals={journals} virtual_mib={} companions_taken={}",
            bytes.div_ceil(1 << 20),
            self.companions_taken
        ));
        lines.extend(self.core.status_lines());
        let mut text = lines.join("\n");
        text.truncate(text.floor_char_boundary(crate::wire::MAX_STATUS));
        text
    }

    /// The core, for in-process tests.
    #[must_use]
    pub fn core(&self) -> &KeeperCore {
        &self.core
    }

    /// Whether a custody copy of `rdev` is held.
    #[must_use]
    pub fn holds(&self, rdev: Rdev) -> bool {
        self.custody.contains_key(&rdev)
    }

    /// A test seam: end every connection as a read error or a failed write does, while the peers'
    /// processes live on. Each socket is shut down before it is dropped: a mere close leaves the
    /// connection open while another process still holds the descriptor (a concurrent test's
    /// child spawned before it was close-on-exec).
    #[cfg(all(test, target_vendor = "apple"))]
    pub(crate) fn test_drop_connections(&mut self) {
        let ids: Vec<ConnId> = self.conns.keys().copied().collect();
        for id in ids {
            if let Some(c) = self.conns.get(&id) {
                let _ = c.stream.shutdown(std::net::Shutdown::Both);
            }
            self.drop_conn(id);
        }
    }

    /// Why this keeper turned each dial away unchecked (its accept check could
    /// not run), never cleared by a later admitted dial: a test tells a
    /// `trustd` that went silent from a verdict on the dialer.
    #[cfg(all(test, target_vendor = "apple"))]
    pub(crate) fn test_unchecked_ever(&self) -> &[String] {
        &self.test_unchecked_ever
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// Create `dir` if missing (0700) and require it to be a real directory of
/// ours that no one else can write.
pub(crate) fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new().mode(0o700).create(dir)?;
        }
        Err(e) => return Err(e),
        Ok(_) => {}
    }
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.file_type().is_dir()
        || meta.uid() != aterm_uds::peer::our_uid()
        || meta.permissions().mode() & 0o022 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not a private directory of ours", dir.display()),
        ));
    }
    Ok(())
}

/// After the bind: the node is a socket of ours inside our real directory.
fn prove_socket_is_ours(dir: &Path, path: &Path) -> io::Result<()> {
    ensure_private_dir(dir)?;
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.file_type().is_socket() || meta.uid() != aterm_uds::peer::our_uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the bound node is not a socket of ours",
        ));
    }
    Ok(())
}

#[cfg(all(test, target_vendor = "apple"))]
mod tests {
    use super::*;
    use crate::core::RecordState;
    use crate::wire::{CAP_SENDS_BYE, PeerClass};

    #[test]
    fn a_status_limit_inside_a_multibyte_character_keeps_valid_utf8() {
        let dir = std::env::temp_dir().join(format!("akk-unit-{}-status-utf8", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut server = Server::bind(ServeConfig {
            socket: dir.join("k.sock"),
            identity: Identity::same_uid(),
            relaunch: Relauncher::Unavailable,
            version: "test".into(),
            test_drop_custody: false,
        })
        .expect("bind");
        let prefix = format!("keeper=running\npid={} version=", server.own_pid);
        server.version = format!(
            "{}é",
            "x".repeat(crate::wire::MAX_STATUS - prefix.len() - 1)
        );
        let text = server.status_text();
        assert_eq!(text.len(), crate::wire::MAX_STATUS - 1);
        assert!(text.starts_with(&prefix));
        assert!(Frame::Status { text }.encode().is_ok());
        drop(server);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A peer wrote HELLO, a claim and BYE, then closed its end before the keeper read any: the
    /// WELCOME is lost, the claim and the BYE are still read, and the EOF drops the connection.
    /// The peer is this live process, so its end is an EOF, not a death: the claim stays.
    #[test]
    fn a_failed_write_ends_the_writing_never_the_reading() {
        let dir = std::env::temp_dir().join(format!("akk-unit-{}-unwritable", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut server = Server::bind(ServeConfig {
            socket: dir.join("k.sock"),
            identity: Identity::same_uid(),
            relaunch: Relauncher::Unavailable,
            version: "test".into(),
            test_drop_custody: false,
        })
        .expect("bind");
        let client = CtlStream::connect(server.socket()).expect("connect");
        // The accept (the kernel names the peer only while it is connected).
        server.step(0).expect("turn");
        assert_eq!(server.conns.len(), 1, "accepted");
        let null = std::fs::File::open("/dev/null").expect("a character device");
        let rdev = null.metadata().expect("stat").rdev();
        let me = std::process::id();
        let frames = [
            Frame::Hello {
                proto: PROTO,
                class: PeerClass::App,
                caps: CAP_SENDS_BYE,
                build: "test".into(),
                marker: None,
            },
            Frame::Register {
                header: MasterHeader {
                    rdev,
                    shell_pid: me,
                    shell_birth: born(me).expect("our birth"),
                    local_id: 1,
                },
                tag: b"sid=s-unit".to_vec(),
            },
            Frame::Bye,
        ];
        for frame in &frames {
            let fd = matches!(frame, Frame::Register { .. }).then(|| null.as_fd());
            write_frame(&client, frame, fd).expect("write");
        }
        drop(client);
        // The peer's end closes only when every copy of it does, and a child a sibling test
        // thread forked and has not yet exec'd holds one for a while: wait for the kernel's
        // hang-up, not the drop.
        let fd = server
            .conns
            .values()
            .next()
            .expect("the connection")
            .stream
            .as_raw_fd();
        let started = Instant::now();
        loop {
            let mut pfd = [PollFd {
                fd,
                events: POLLIN,
                revents: 0,
            }];
            let _ = sys::poll(&mut pfd, 0);
            if pfd[0].revents & POLLHUP != 0 {
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(60), "never hung up");
            std::thread::sleep(Duration::from_millis(1));
        }
        // One frame per connection per turn: HELLO, REGISTER, BYE, then EOF.
        for _ in 0..4 {
            server.step(0).expect("turn");
        }
        assert_eq!(server.replies_lost, 1, "the WELCOME was not written");
        let (conn, peer) = server
            .core()
            .peers()
            .iter()
            .next()
            .expect("the peer, alive, is remembered");
        assert!(peer.bye, "the BYE behind the unwritable WELCOME was read");
        let rec = server
            .core()
            .records()
            .get(&rdev)
            .expect("the claim behind it was read");
        assert_eq!(rec.claimants, vec![*conn]);
        assert!(server.holds(rdev), "and its descriptor kept");
        assert!(server.conns.is_empty(), "the EOF dropped the connection");
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A fresh PTY master no other process holds, and its rdev (`aterm_pty::open_pty_pair`:
    /// close-on-exec from the open, so a sibling test's child cannot hold it, and redriven when
    /// XNU's `/dev/ptmx` race answers `-6`). The slave is closed at once; the keeper holds and
    /// offers masters only.
    fn a_pty_master() -> (OwnedFd, Rdev) {
        use std::os::fd::FromRawFd as _;
        let (master, slave) = aterm_pty::open_pty_pair().expect("a fresh PTY pair");
        // SAFETY: two fresh descriptors this function alone owns.
        let (master, slave) =
            unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        drop(slave);
        let file = std::fs::File::from(master);
        let rdev = file.metadata().expect("fstat").rdev();
        (OwnedFd::from(file), rdev)
    }

    fn hello_app() -> Frame {
        Frame::Hello {
            proto: PROTO,
            class: PeerClass::App,
            caps: CAP_SENDS_BYE,
            build: "test".into(),
            marker: None,
        }
    }

    fn claim(rdev: Rdev, local_id: u64) -> Frame {
        let me = std::process::id();
        Frame::Register {
            header: MasterHeader {
                rdev,
                shell_pid: me,
                shell_birth: born(me).expect("our birth"),
                local_id,
            },
            tag: b"sid=s-unit".to_vec(),
        }
    }

    /// Turn until the one connection's EOF drops it. The peer's end is closed
    /// only when every copy of it is: a child a sibling test thread has
    /// forked and not yet exec'd holds one for a while.
    fn until_its_eof(server: &mut Server) {
        let started = Instant::now();
        while !server.conns.is_empty() {
            assert!(started.elapsed() < HANG, "no EOF");
            server.step(10).expect("turn");
        }
    }

    const HANG: Duration = Duration::from_secs(60);

    /// What the second window of [`offer_to_a_second_window`] saw and left.
    struct OfferSeen {
        /// The frames it read, in order, up to the keeper's EOF (or the second
        /// frame after the WELCOME), with each OFFER's descriptor's rdev.
        read: Vec<(Frame, Option<Rdev>)>,
        /// The master window A left orphaned.
        orphan_rdev: Rdev,
        bye_read: bool,
        claim_read: bool,
        /// The orphan is Orphaned again after B's EOF: an offer is made to a
        /// connection, and returns when that connection is gone.
        returned_at_eof: bool,
        replies_lost: u64,
        dropped_at_eof: bool,
    }

    /// Window A claims a master and crashes; window B says HELLO — offered
    /// A's master — then claims its own and says BYE, all written before the
    /// keeper reads any of it. `fail_dup` makes the OFFER's duplicate fail.
    fn offer_to_a_second_window(fail_dup: bool) -> OfferSeen {
        let me = std::process::id();
        let dir =
            std::env::temp_dir().join(format!("akk-unit-{me}-undupable-{}", u8::from(fail_dup)));
        let _ = std::fs::remove_dir_all(&dir);
        let mut server = Server::bind(ServeConfig {
            socket: dir.join("k.sock"),
            identity: Identity::same_uid(),
            relaunch: Relauncher::Unavailable,
            version: "test".into(),
            test_drop_custody: false,
        })
        .expect("bind");

        // Window A: HELLO and a claim, then its EOF (one frame per turn).
        let (orphan, orphan_rdev) = a_pty_master();
        let a = CtlStream::connect(server.socket()).expect("connect A");
        server.step(0).expect("accept A");
        write_frame(&a, &hello_app(), None).expect("hello A");
        write_frame(&a, &claim(orphan_rdev, 1), Some(orphan.as_fd())).expect("claim A");
        for _ in 0..2 {
            server.step(0).expect("turn");
        }
        drop(a);
        until_its_eof(&mut server);
        // A's exit as its watch reports it: killed (SIGKILL). The peer is this
        // process, which does not exit, so the watch's report is handed to
        // the core the way `read_watch` hands it.
        let now = server.now();
        let mut env = server.env();
        let outs = server.core.peer_exit(me, 9, now, &mut env);
        server.apply(outs);
        // A crashed: its master is an orphan. (A sibling test's child, between its fork and
        // exec, can briefly hold this process's descriptors: the death's holder scan then makes
        // it a handoff, and the hold's rescan orphans the master once the child is gone.)
        let started = Instant::now();
        while server.core().records()[&orphan_rdev].state != RecordState::Orphaned {
            assert!(started.elapsed() < HANG, "A's master never orphaned");
            server.step(10).expect("turn");
        }

        // Window B: HELLO, its own claim, BYE — all written before any read.
        // The same moment's holder, seen by the offer's scan, leaves the orphan
        // unoffered: such a window is void, and the next one tries again.
        let started = Instant::now();
        let (b, b_id, own_rdev) = loop {
            assert!(started.elapsed() < HANG, "the orphan was never offered");
            let b = CtlStream::connect(server.socket()).expect("connect B");
            b.set_read_timeout(Some(HANG)).expect("B's hang detector");
            server.step(0).expect("accept B");
            let b_id = *server.conns.keys().next_back().expect("B accepted");
            let (own, own_rdev) = a_pty_master();
            write_frame(&b, &hello_app(), None).expect("hello B");
            write_frame(&b, &claim(own_rdev, 1), Some(own.as_fd())).expect("claim B");
            write_frame(&b, &Frame::Bye, None).expect("bye B");
            server.test_fail_next_dup = fail_dup;
            // HELLO (answered: WELCOME, OFFER), REGISTER, BYE.
            for _ in 0..3 {
                server.step(0).expect("turn");
            }
            let offered = server
                .core()
                .records()
                .get(&orphan_rdev)
                .is_some_and(|r| r.state == RecordState::Offered { to: b_id });
            if offered {
                break (b, b_id, own_rdev);
            }
            server.test_fail_next_dup = false;
            drop(b);
            until_its_eof(&mut server);
        };
        assert!(!server.test_fail_next_dup, "the OFFER's duplicate was made");
        let mut read = Vec::new();
        while read.len() < 2 {
            match read_frame(&b).expect("B reads") {
                Some(Incoming { frame, fd }) => {
                    let rdev =
                        fd.map(|fd| std::fs::File::from(fd).metadata().expect("fstat").rdev());
                    read.push((frame, rdev));
                }
                None => break,
            }
        }
        let peer = server.core().peers().get(&b_id);
        let seen = OfferSeen {
            read,
            orphan_rdev,
            bye_read: peer.is_some_and(|p| p.bye),
            claim_read: server
                .core()
                .records()
                .get(&own_rdev)
                .is_some_and(|r| r.claimants == vec![b_id]),
            returned_at_eof: false,
            replies_lost: server.replies_lost,
            dropped_at_eof: false,
        };
        let open_before_eof = server.conns.contains_key(&b_id);
        drop(b);
        until_its_eof(&mut server);
        let seen = OfferSeen {
            dropped_at_eof: open_before_eof,
            returned_at_eof: server
                .core()
                .records()
                .get(&orphan_rdev)
                .is_some_and(|r| r.state == RecordState::Orphaned),
            ..seen
        };
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
        seen
    }

    /// The duplicate fails as `EMFILE` would: the recipient reads the WELCOME then EOF, its claim
    /// and BYE are still read, and the offer returns to Orphaned at its EOF. Negative control:
    /// with the duplicate made, the OFFER carrying the orphan's master follows the WELCOME.
    #[test]
    fn an_offer_that_cannot_be_duplicated_ends_the_writing_never_the_reading() {
        let failed = offer_to_a_second_window(true);
        assert!(
            failed.claim_read,
            "the claim behind the unwritten OFFER was read"
        );
        assert!(
            failed.bye_read,
            "the BYE behind the unwritten OFFER was read"
        );
        assert!(failed.dropped_at_eof, "the connection lived until its EOF");
        assert!(
            failed.returned_at_eof,
            "the offer returns to Orphaned at B's EOF, never left with a dead link"
        );
        assert_eq!(failed.replies_lost, 1, "the OFFER was not written");
        assert!(
            matches!(
                failed.read.as_slice(),
                [(Frame::Welcome { offers: 1, .. }, None)]
            ),
            "B read the WELCOME, then EOF: {:?}",
            failed.read
        );

        let control = offer_to_a_second_window(false);
        assert!(control.claim_read && control.bye_read && control.dropped_at_eof);
        assert_eq!(control.replies_lost, 0);
        assert!(
            matches!(
                control.read.as_slice(),
                [
                    (Frame::Welcome { offers: 1, .. }, None),
                    (Frame::Offer { .. }, Some(r))
                ] if *r == control.orphan_rdev
            ),
            "the control's OFFER carries a descriptor: {:?}",
            control.read
        );
    }

    /// The keeper takes in custody only a read-only POSIX shared-memory object of the kind and
    /// length claimed, within the bounds; everything else is named, and nothing is mapped.
    #[test]
    fn a_companion_must_be_a_read_only_region_of_its_claimed_length() {
        use crate::wire::COMPANION_JOURNAL as JOURNAL;
        use aterm_uds::shm::{MAX_LEN, PAGE, Region, testing};
        let len = 2 * PAGE;
        #[expect(
            clippy::disallowed_methods,
            reason = "a test playing the window makes the region the keeper is sent"
        )]
        // SAFETY: companion_fault only inspects descriptor metadata; this custody stays
        // local and neither it nor an alias modifies or resizes the object.
        let (_region, custody) = unsafe { Region::create(len) }.expect("a region");
        let ro = custody.as_fd();
        assert_eq!(
            companion_fault(ro, JOURNAL, len as u64),
            None,
            "the real thing"
        );

        #[expect(
            clippy::disallowed_methods,
            reason = "a test playing the window makes the region the keeper is sent"
        )]
        // SAFETY: companion_fault reads only descriptor metadata and refuses writable
        // custody; this private descriptor is never written, resized or mapped.
        let (_twin, writable) =
            unsafe { testing::create_with_writable_custody(len) }.expect("a twin");
        let why = companion_fault(writable.as_fd(), JOURNAL, len as u64).expect("refused");
        assert!(why.contains("open for writing"), "{why}");
        let why = companion_fault(ro, JOURNAL, 3 * PAGE as u64).expect("refused");
        assert!(why.contains("not the"), "{why}");
        let why = companion_fault(ro, 2, len as u64).expect("refused");
        assert!(why.contains("no journal"), "{why}");
        for bad in [0, PAGE as u64 + 1, (MAX_LEN + PAGE) as u64] {
            let why = companion_fault(ro, JOURNAL, bad).expect("refused");
            assert!(why.contains("pages"), "{bad}: {why}");
        }
        let null = std::fs::File::open("/dev/null").expect("/dev/null");
        let why = companion_fault(null.as_fd(), JOURNAL, len as u64).expect("refused");
        assert!(why.contains("not POSIX shared memory"), "{why}");
    }

    /// The loop's descriptors follow the core's decisions exactly: after a COMPANION, a second
    /// one that supersedes it (`Out::CloseCompanion`), and the RELEASE that removes the record
    /// (`Out::Close`, which closes every companion beside the master), the regions the loop
    /// holds are the core's, the status counts them from the descriptors (`fstat`), and the
    /// kernel's own table of this process shows exactly that many of the region's size. A
    /// teardown that forgot one would leave the region pinned for the keeper's life with every
    /// status line still clean, were they counted from the core.
    #[test]
    fn the_loop_holds_exactly_the_regions_the_core_keeps() {
        use aterm_uds::shm::{PAGE, Region};
        // A size no other test in this binary makes: they run beside this one, in-process.
        let len = 7 * PAGE;
        let dir = std::env::temp_dir().join(format!("akk-unit-{}-regions", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut server = Server::bind(ServeConfig {
            socket: dir.join("k.sock"),
            identity: Identity::same_uid(),
            relaunch: Relauncher::Unavailable,
            version: "test".into(),
            test_drop_custody: false,
        })
        .expect("bind");
        let client = CtlStream::connect(server.socket()).expect("connect");
        server.step(0).expect("the accept");
        let null = std::fs::File::open("/dev/null").expect("a character device");
        let rdev = null.metadata().expect("stat").rdev();
        let me = std::process::id();
        let held_here = || {
            aterm_uds::holders::pshm_sizes_of(me)
                .expect("our own table")
                .into_iter()
                .filter(|s| *s == len as u64)
                .count()
        };
        let in_step = |server: &Server| {
            let loop_holds: std::collections::BTreeSet<CompanionId> =
                server.companions.keys().copied().collect();
            assert_eq!(loop_holds, server.core().companions_held());
            loop_holds.len()
        };
        let send = |server: &mut Server, frame: Frame, fd: Option<std::os::fd::BorrowedFd<'_>>| {
            write_frame(&client, &frame, fd).expect("write");
            server.step(0).expect("turn");
        };
        let hello = Frame::Hello {
            proto: PROTO,
            class: PeerClass::App,
            caps: CAP_SENDS_BYE | CAP_JOURNAL,
            build: "test".into(),
            marker: None,
        };
        send(&mut server, hello, None);
        let register = Frame::Register {
            header: MasterHeader {
                rdev,
                shell_pid: me,
                shell_birth: born(me).expect("our birth"),
                local_id: 1,
            },
            tag: b"sid=s-unit".to_vec(),
        };
        send(&mut server, register, Some(null.as_fd()));
        let companion = Frame::Companion {
            rdev,
            kind: COMPANION_JOURNAL,
            len: len as u64,
        };
        for taken in 1..=2u64 {
            // The window's own copies close once sent: the loop's is then the only one here.
            #[expect(
                clippy::disallowed_methods,
                reason = "a test playing the window makes the region the keeper is sent"
            )]
            // SAFETY: the fixture server only queries/holds/closes custody, never maps,
            // reopens or resizes it; the inert local region is dropped after the send.
            let (region, custody) = unsafe { Region::create(len) }.expect("a region");
            send(&mut server, companion.clone(), Some(custody.as_fd()));
            drop((region, custody));
            assert_eq!(in_step(&server), 1, "the second supersedes the first");
            assert_eq!(held_here(), 1, "the kernel agrees");
            assert!(
                server.status_text().contains(&format!(
                    "journals=1 virtual_mib=1 companions_taken={taken}"
                )),
                "the second is counted though the count held stays 1: {}",
                server.status_text()
            );
        }
        send(&mut server, Frame::Release { rdev }, None);
        assert!(!server.holds(rdev), "the master closed");
        assert_eq!(in_step(&server), 0, "and every region beside it");
        assert_eq!(held_here(), 0, "the kernel agrees");
        assert!(
            server.status_text().contains("journals=0 virtual_mib=0"),
            "{}",
            server.status_text()
        );
        drop(client);
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
