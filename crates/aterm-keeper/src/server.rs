// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Single-threaded `poll(2)` server loop driving [`crate::core::KeeperCore`] over a Unix socket.

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
use crate::identity::{CheckError, Identity, PeerCheck};
use crate::sys::{self, POLLERR, POLLHUP, POLLIN, PollFd};
use crate::wire::{
    Birth, CAP_JOURNAL, CAP_SYNC, COMPANION_JOURNAL, Frame, JournalOffer, MarkerRef, MasterHeader,
    PROTO, RefuseCode,
};

/// Mechanism used to relaunch the terminal application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Relauncher {
    /// Relaunching is unavailable (running outside an app bundle).
    Unavailable,
    /// Relaunches via `/usr/bin/open -a <bundle>`.
    OpenBundle(PathBuf),
    /// Library-only test seam that spawns the given command argv.
    TestCommand(Vec<String>),
}

/// Configuration for [`Server::bind`].
#[derive(Debug)]
pub struct ServeConfig {
    /// Unix socket path to bind inside a private directory.
    pub socket: PathBuf,
    /// Peer identity required on incoming connections.
    pub identity: Identity,
    /// Relaunch mechanism.
    pub relaunch: Relauncher,
    /// Keeper version string reported in `WELCOME` and status output.
    pub version: String,
    /// Test seam that closes registered masters immediately instead of keeping custody.
    pub test_drop_custody: bool,
}

struct Dialing {
    stream: CtlStream,
    check: PeerCheck,
    until: Instant,
}

struct Conn {
    stream: CtlStream,
    pid: u32,
    /// A frame to this peer could not be written ([`Server::send`]).
    unwritable: bool,
}

/// Keeper socket server wrapping [`KeeperCore`].
pub struct Server {
    listener: UnixListener,
    socket: PathBuf,
    core: KeeperCore,
    conns: BTreeMap<ConnId, Conn>,
    /// Dials whose identity checks are running.
    dialing: Vec<Dialing>,
    /// The most checks running at once; later dials wait in the listen backlog.
    dial_checks_max: usize,
    /// How long one dial's check may run before the dial is turned away as unanswered.
    check_hang: Duration,
    custody: BTreeMap<Rdev, OwnedFd>,
    /// Every crash-restore companion held.
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
    /// COMPANIONs this keeper accepted since it started (`status companions_taken=`).
    companions_taken: u64,
    /// Why the last dial was refused unchecked.
    unchecked_said: Option<String>,
    /// Frames [`Server::drain_pid`] handed the core behind their writer's exit: those the turn's
    /// own read had not taken when the exit watch delivered.
    frames_behind_exit: u64,
    /// Frames the keeper could not write to their peer ([`Server::send`]).
    replies_lost: u64,
    /// A test seam (unit tests only): the next OFFER's duplicate fails as `EMFILE` would.
    #[cfg(test)]
    test_fail_next_dup: bool,
    /// A test seam.
    #[cfg(test)]
    test_unchecked_ever: Vec<String>,
    /// A test seam: the check each dial gets in place of [`Identity::start_check_named`].
    #[cfg(test)]
    test_check: Option<Box<dyn Fn() -> PeerCheck + Send>>,
}

const IO_TIMEOUT: Duration = Duration::from_secs(2);
const TICK_MS: i32 = 250;
const CHECK_TICK_MS: i32 = 5;
const DIAL_CHECKS_MAX: usize = 16;
/// The most frames one connection's read takes at a time ([`Server::read_waiting`]): in a turn
/// whose poll found it readable, and behind a peer's exit. A turn costs a poll over every
/// connection and exit watch and a look at each watch, so taking one frame a turn made a
/// window's burst (a recovery's REGISTER, COMPANION and SYNCs for each of its sessions) cost
/// sessions × watches.
const DRAIN_MAX_FRAMES: usize = 1024;

struct Env<'a> {
    own_pid: u32,
    peer_pids: Vec<u32>,
    _p: std::marker::PhantomData<&'a ()>,
}

impl Env<'_> {
    fn verdict(&self, scan: aterm_uds::holders::HolderScan) -> Holders {
        if !scan.holders.is_empty() {
            Holders::Held(scan.holders)
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

/// Reads and validates the crash marker of window `pid`.
#[must_use]
pub fn read_marker(pid: u32, marker: &MarkerRef) -> MarkerEvidence {
    use std::os::unix::ffi::OsStrExt as _;
    if marker.dir.contains(&0)
        || marker
            .dir
            .split(|b| *b == b'/')
            .any(|seg| seg == b"." || seg == b"..")
    {
        return MarkerEvidence::Unknown;
    }
    let dir = Path::new(std::ffi::OsStr::from_bytes(&marker.dir));
    if !dir.is_absolute()
        || !dir.components().all(|c| {
            matches!(
                c,
                std::path::Component::RootDir | std::path::Component::Normal(_)
            )
        })
    {
        return MarkerEvidence::Unknown;
    }
    let our_uid = aterm_uds::peer::our_uid();
    match std::fs::symlink_metadata(dir) {
        Ok(m) if m.file_type().is_dir() && m.uid() == our_uid => {}
        _ => return MarkerEvidence::Unknown,
    }
    for owner in ["app", "other", "lab"] {
        let name = format!("crash-marker-{pid}-{}-{owner}.log", marker.nanos);
        for (file, seen) in [(name.clone(), false), (format!("{name}.seen"), true)] {
            let path = dir.join(file);
            match std::fs::symlink_metadata(&path) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Ok(m) if m.file_type().is_file() && m.uid() == our_uid => {
                    if seen {
                        return MarkerEvidence::Dead;
                    }
                    return match aterm_uds::ownerlock::probe(&path) {
                        aterm_uds::ownerlock::Owner::Live => MarkerEvidence::Held,
                        aterm_uds::ownerlock::Owner::Dead => MarkerEvidence::Dead,
                        aterm_uds::ownerlock::Owner::Unknown => MarkerEvidence::Unknown,
                    };
                }
                _ => return MarkerEvidence::Unknown,
            }
        }
    }
    MarkerEvidence::Released
}

/// Returns a refusal reason if `fd` is not a valid read-only companion shared-memory region.
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

/// Returns the kernel start time of `pid`.
#[must_use]
pub fn born(pid: u32) -> Option<Birth> {
    let b = aterm_uds::process::read_process_birth(i32::try_from(pid).ok()?)?;
    Some(Birth {
        seconds: b.seconds,
        micros: u32::try_from(b.microseconds).ok()?,
    })
}

impl Server {
    /// Binds the keeper listener socket according to `config`.
    /// # Errors
    /// Returns an error if the directory is not private, a non-socket or foreign node occupies the
    /// path, a live keeper holds the socket, or binding fails.
    pub fn bind(config: ServeConfig) -> io::Result<Self> {
        let dir = config
            .socket
            .parent()
            .ok_or_else(|| io::Error::other("the socket path has no directory"))?;
        ensure_private_dir(dir)?;
        match std::fs::symlink_metadata(&config.socket) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
            Ok(meta) => {
                if !meta.file_type().is_socket() {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("{} exists and is not a socket", config.socket.display()),
                    ));
                }
                if meta.uid() != aterm_uds::peer::our_uid() {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!("{} is not a socket of ours", config.socket.display()),
                    ));
                }
                if CtlStream::connect(&config.socket).is_ok() {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        format!("a keeper already answers on {}", config.socket.display()),
                    ));
                }
                std::fs::remove_file(&config.socket)?;
            }
        }
        // The keeper spawns every session's program: keep its sockets out of them all.
        let listener = aterm_uds::kept_out(|| UnixListener::bind(&config.socket))?;
        prove_socket_is_ours(dir, &config.socket)?;
        sys::raise_nofile();
        let can_relaunch = config.relaunch != Relauncher::Unavailable;
        Ok(Self {
            listener,
            socket: config.socket,
            core: KeeperCore::new(can_relaunch),
            conns: BTreeMap::new(),
            dialing: Vec::new(),
            dial_checks_max: DIAL_CHECKS_MAX,
            check_hang: crate::identity::peer_wait(aterm_uds::peer::CHECK_HANG),
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
            #[cfg(test)]
            test_check: None,
        })
    }

    /// Returns the bound socket path.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Runs the server loop until terminated.
    /// # Errors
    /// Returns an error if `poll` fails for a reason other than `EINTR`.
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

    /// Runs one server turn waiting up to `timeout_ms`.
    /// # Errors
    /// Returns an error if `poll` fails for a reason other than `EINTR`.
    pub fn step(&mut self, timeout_ms: i32) -> io::Result<()> {
        self.turn(timeout_ms, || {})
    }

    /// A test seam: [`Server::step`], calling `park` once the turn's poll has returned and before
    /// the turn reads anything. What a peer does while it is parked (writes its frames and exits)
    /// lands after the poll and before the turn's sweep of its exit watches, so the turn's own
    /// read skips that connection and only [`Server::drain_pid`] reads those frames before the
    /// death is judged.
    /// # Errors
    /// Returns an error if `poll` fails for a reason other than `EINTR`.
    #[cfg(any(test, debug_assertions))]
    pub fn step_parked(&mut self, timeout_ms: i32, park: impl FnOnce()) -> io::Result<()> {
        self.turn(timeout_ms, park)
    }

    fn turn(&mut self, timeout_ms: i32, park: impl FnOnce()) -> io::Result<()> {
        let accepting = self.dialing.len() < self.dial_checks_max;
        let mut fds = vec![PollFd {
            fd: self.listener.as_raw_fd(),
            events: if accepting { POLLIN } else { 0 },
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
        match sys::poll(&mut fds, self.poll_wait(timeout_ms)) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
        park();
        let ready = |pfd: &PollFd| pfd.revents & (POLLIN | POLLHUP | POLLERR) != 0;
        if accepting && ready(&fds[0]) {
            self.accept_all();
        }
        self.admit_checked();
        for (i, id) in conn_ids.iter().enumerate() {
            if ready(&fds[1 + i]) {
                self.read_waiting(*id, DRAIN_MAX_FRAMES);
            }
        }
        for (i, pid) in watch_pids.iter().enumerate() {
            if ready(&fds[1 + conn_ids.len() + i]) {
                self.read_watch(*pid);
            }
        }
        // An exit that arrived after the poll: its connections' frames since are drained there.
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

    fn poll_wait(&self, timeout_ms: i32) -> i32 {
        if self.dialing.is_empty() || (0..=CHECK_TICK_MS).contains(&timeout_ms) {
            timeout_ms
        } else {
            CHECK_TICK_MS
        }
    }

    fn accept_all(&mut self) {
        while self.dialing.len() < self.dial_checks_max {
            let stream = match aterm_uds::accept(
                &self.listener,
                aterm_uds::AcceptWait::Ready,
                aterm_uds::AcceptedMode::Blocking,
            ) {
                Ok((s, _)) => s,
                Err(_) => return,
            };
            let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
            let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
            let check = self.start_check(&stream);
            let until = Instant::now() + self.check_hang;
            self.dialing.push(Dialing {
                stream,
                check,
                until,
            });
        }
    }

    fn start_check(&self, stream: &CtlStream) -> PeerCheck {
        #[cfg(test)]
        if let Some(start) = &self.test_check {
            return start();
        }
        self.identity
            .start_check_named(stream, aterm_uds::peer::peer_audit_token(stream))
    }

    fn admit_checked(&mut self) {
        let now = Instant::now();
        for Dialing {
            stream,
            check,
            until,
        } in std::mem::take(&mut self.dialing)
        {
            match check.wait(Duration::ZERO) {
                Ok(Ok(())) => self.admit(stream),
                Ok(Err(e)) => self.turn_away(e),
                Err(check) if now >= until => {
                    self.turn_away(CheckError::Unanswered(check.unanswered()));
                }
                Err(check) => self.dialing.push(Dialing {
                    stream,
                    check,
                    until,
                }),
            }
        }
    }

    fn turn_away(&mut self, e: CheckError) {
        self.refused_dials += 1;
        if let CheckError::Unchecked(why) | CheckError::Unanswered(why) = e {
            #[cfg(test)]
            self.test_unchecked_ever.push(why.clone());
            if self.unchecked_said.as_deref() != Some(why.as_str()) {
                eprintln!(
                    "keeper: turned away a connection whose identity could not be verified: {why}"
                );
                self.unchecked_said = Some(why);
            }
        }
    }

    fn admit(&mut self, stream: CtlStream) {
        self.unchecked_said = None;
        let Ok(pid) = aterm_uds::fdpass::peer_pid(&stream) else {
            self.refused_dials += 1;
            return;
        };
        let birth = born(pid);
        let id = self.next_conn;
        self.next_conn += 1;
        if !self.core.accept(id, pid, birth) {
            self.refused_dials += 1;
            return;
        }
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

    /// Hands the core every complete frame `pid` wrote that the turn's own read has not taken, on
    /// each of its connections, before [`Server::read_watch`] judges its exit: a REGISTER or BYE
    /// that arrived after the turn's poll, beside an exit the sweep found, is never read as
    /// coming after the death. Each counts in `frames_behind_exit`.
    fn drain_pid(&mut self, pid: u32) {
        let ids: Vec<ConnId> = self
            .conns
            .iter()
            .filter(|(_, c)| c.pid == pid)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            let read = self.read_waiting(id, DRAIN_MAX_FRAMES);
            self.frames_behind_exit = self
                .frames_behind_exit
                .saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        }
    }

    /// Read and handle the frames already waiting on connection `id`, up to `max` of them, in
    /// order: until a look finds nothing more to read or the connection is gone. Returns how
    /// many it handled.
    fn read_waiting(&mut self, id: ConnId, max: usize) -> usize {
        let mut read = 0;
        for _ in 0..max {
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
                        read += 1;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                _ => break,
            }
        }
        read
    }

    fn read_conn(&mut self, id: ConnId) -> bool {
        let Some(conn) = self.conns.get(&id) else {
            return false;
        };
        let incoming = match read_frame(&conn.stream) {
            Ok(Some(i)) => i,
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
            Frame::Sync { seq } => {
                self.send(id, &Frame::Sync { seq }, None);
                Vec::new()
            }
            Frame::RestartWhenSafe => Vec::new(),
            Frame::Companion { rdev, kind, len } => self.companion(id, rdev, kind, len, fd),
            Frame::CompanionRelease { rdev, kind } => self.core.companion_release(id, rdev, kind),
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
        drop(fd);
        outs
    }

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
                        identity: Some(self.identity.policy()),
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

    fn send(&mut self, conn: ConnId, frame: &Frame, fd: Option<&OwnedFd>) {
        let Some(c) = self.conns.get(&conn) else {
            return;
        };
        if c.unwritable || write_frame(&c.stream, frame, fd.map(|f| f.as_fd())).is_err() {
            self.end_writing(conn);
        }
    }

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
        if let Ok(pid) = spawned {
            self.children.push(pid);
        }
    }

    /// Formats the keeper status report.
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

    /// Borrows the underlying [`KeeperCore`].
    #[must_use]
    pub const fn core(&self) -> &KeeperCore {
        &self.core
    }

    /// Returns `true` if the server currently holds a custody descriptor for `rdev`.
    #[must_use]
    pub fn holds(&self, rdev: Rdev) -> bool {
        self.custody.contains_key(&rdev)
    }

    /// Drops all active client connections for testing.
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

    /// Returns reasons why incoming connections were rejected before identity verification.
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

/// Creates `dir` with mode `0700` if missing and verifies it is an owner-private directory.
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
#[path = "server_tests.rs"]
mod tests;
