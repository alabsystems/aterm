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
    /// Frames read after their writer's exit had already arrived.
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
    let dir = Path::new(std::ffi::OsStr::from_bytes(&marker.dir));
    if !dir.is_absolute() {
        return MarkerEvidence::Unknown;
    }
    match std::fs::symlink_metadata(dir) {
        Ok(m) if m.file_type().is_dir() && m.uid() == aterm_uds::peer::our_uid() => {}
        _ => return MarkerEvidence::Unknown,
    }
    for owner in ["app", "other", "lab"] {
        let name = format!("crash-marker-{pid}-{}-{owner}.log", marker.nanos);
        for (file, seen) in [(name.clone(), false), (format!("{name}.seen"), true)] {
            let path = dir.join(file);
            match std::fs::symlink_metadata(&path) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Ok(m) if m.file_type().is_file() => {
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
    /// Returns an error if the directory is not private, a live keeper holds the socket, or binding
    /// fails.
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
            dialing: Vec::new(),
            dial_checks_max: DIAL_CHECKS_MAX,
            check_hang: crate::identity::PEER_CHECK_HANG,
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
        let ready = |pfd: &PollFd| pfd.revents & (POLLIN | POLLHUP | POLLERR) != 0;
        if accepting && ready(&fds[0]) {
            self.accept_all();
        }
        self.admit_checked();
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
            let stream = match self.listener.accept() {
                Ok((s, _)) => s,
                Err(_) => return,
            };
            let _ = stream.set_nonblocking(false);
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
mod tests {
    use super::*;
    use crate::core::RecordState;
    use crate::wire::{CAP_SENDS_BYE, PeerClass};

    #[test]
    fn a_lab_desks_marker_is_read_as_any_marker_is() {
        use std::os::unix::ffi::OsStrExt as _;
        let dir = std::env::temp_dir().join(format!("akk-unit-{}-lab-marker", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the scratch log dir");
        let pid = 4242;
        let m = MarkerRef {
            nanos: 7,
            dir: dir.as_os_str().as_bytes().to_vec(),
        };
        assert_eq!(read_marker(pid, &m), MarkerEvidence::Released, "none yet");
        let marker = dir.join(format!("crash-marker-{pid}-7-lab.log"));
        std::fs::write(&marker, b"").expect("a dead lab marker");
        assert_eq!(read_marker(pid, &m), MarkerEvidence::Dead);
        let held = std::fs::File::open(&marker).expect("open the marker");
        #[expect(
            clippy::disallowed_methods,
            reason = "the owner lock a live lab desk holds, let go when the file drops below"
        )]
        held.try_lock().expect("the owner lock");
        assert_eq!(read_marker(pid, &m), MarkerEvidence::Held);
        drop(held);
        let seen = dir.join(format!("crash-marker-{pid}-7-lab.log.seen"));
        std::fs::rename(&marker, &seen).expect("consumed");
        assert_eq!(read_marker(pid, &m), MarkerEvidence::Dead, "a consumed one");
        let _ = std::fs::remove_dir_all(dir);
    }

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

    fn a_pty_master() -> (OwnedFd, Rdev) {
        use std::os::fd::FromRawFd as _;
        let (master, slave) = aterm_pty::open_pty_pair().expect("a fresh PTY pair");
        let (master, slave) =
            // SAFETY: `master` and `slave` are fresh descriptors owned exclusively here.
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

    fn until_its_eof(server: &mut Server) {
        let started = Instant::now();
        while !server.conns.is_empty() {
            assert!(started.elapsed() < HANG, "no EOF");
            server.step(10).expect("turn");
        }
    }

    const HANG: Duration = Duration::from_secs(60);

    struct OfferSeen {
        /// The frames it read.
        read: Vec<(Frame, Option<Rdev>)>,
        /// The master window A left orphaned.
        orphan_rdev: Rdev,
        bye_read: bool,
        claim_read: bool,
        /// The orphan is Orphaned again after B's EOF.
        returned_at_eof: bool,
        replies_lost: u64,
        dropped_at_eof: bool,
    }

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
        let now = server.now();
        let mut env = server.env();
        let outs = server.core.peer_exit(me, 9, now, &mut env);
        server.apply(outs);
        let started = Instant::now();
        while server.core().records()[&orphan_rdev].state != RecordState::Orphaned {
            assert!(started.elapsed() < HANG, "A's master never orphaned");
            server.step(10).expect("turn");
        }

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

    #[test]
    fn a_companion_must_be_a_read_only_region_of_its_claimed_length() {
        use crate::wire::COMPANION_JOURNAL as JOURNAL;
        use aterm_uds::shm::{MAX_LEN, PAGE, Region, testing};
        let len = 2 * PAGE;
        #[expect(
            clippy::disallowed_methods,
            reason = "a test playing the window makes the region the keeper is sent"
        )]
        // SAFETY: `companion_fault` only inspects descriptor metadata; this custody stays local,
        // and neither it nor an alias modifies or resizes the object.
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
        let (_twin, writable) =
            // SAFETY: `companion_fault` only inspects metadata and rejects writable custody; this
            // private descriptor is never written, resized or mapped.
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

    #[test]
    fn the_loop_holds_exactly_the_regions_the_core_keeps() {
        use aterm_uds::shm::{PAGE, Region};
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
            #[expect(
                clippy::disallowed_methods,
                reason = "a test playing the window makes the region the keeper is sent"
            )]
            // SAFETY: the fixture server only queries, holds, and closes custody without mapping or
            // resizing.
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

    /// How the dials of [`dials_whose_checks_need_each_other`] ended.
    #[derive(Debug, PartialEq, Eq)]
    struct DialsEnded {
        /// Dials whose HELLO a WELCOME answered.
        welcomed: usize,
        /// Dials the keeper closed unanswered.
        closed: usize,
        /// The keeper's count of dials turned away.
        refused: u64,
        /// Dials turned away because their check did not answer.
        unanswered: usize,
    }

    fn dials_whose_checks_need_each_other(n: usize, max: usize, hang: Duration) -> DialsEnded {
        use std::sync::{Arc, Condvar, Mutex, PoisonError};
        static SCRIPTED: aterm_uds::deadline::Gate =
            aterm_uds::deadline::Gate::new("akk-unit-scripted-check");
        let dir = std::env::temp_dir().join(format!("akk-unit-{}-dials-{max}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut server = Server::bind(ServeConfig {
            socket: dir.join("k.sock"),
            identity: Identity::same_uid(),
            relaunch: Relauncher::Unavailable,
            version: "test".into(),
            test_drop_custody: false,
        })
        .expect("bind");
        server.dial_checks_max = max;
        server.check_hang = hang;
        let started = Arc::new((Mutex::new(0usize), Condvar::new()));
        server.test_check = Some(Box::new({
            let started = Arc::clone(&started);
            move || {
                let (count, moved) = &*started;
                *count.lock().unwrap_or_else(PoisonError::into_inner) += 1;
                moved.notify_all();
                let started = Arc::clone(&started);
                PeerCheck::start_on(&SCRIPTED, HANG, move || {
                    let (count, moved) = &*started;
                    let count = count.lock().unwrap_or_else(PoisonError::into_inner);
                    let (count, waited) = moved
                        .wait_timeout_while(count, HANG, |running| *running < n)
                        .unwrap_or_else(PoisonError::into_inner);
                    drop(count);
                    if waited.timed_out() {
                        Err(CheckError::Unchecked("the other checks never ran".into()))
                    } else {
                        Ok(())
                    }
                })
            }
        }));
        let mut dials: Vec<Option<CtlStream>> = (0..n)
            .map(|_| {
                let dial = CtlStream::connect(server.socket()).expect("dial");
                write_frame(&dial, &hello_app(), None).expect("hello");
                dial.set_read_timeout(Some(HANG))
                    .expect("the hang detector");
                Some(dial)
            })
            .collect();
        let (mut welcomed, mut closed) = (0, 0);
        let began = Instant::now();
        while dials.iter().any(Option::is_some) {
            assert!(began.elapsed() < HANG, "a dial was never answered");
            server.step(10).expect("turn");
            for slot in &mut dials {
                let Some(dial) = slot else { continue };
                let mut pfd = [PollFd {
                    fd: dial.as_raw_fd(),
                    events: POLLIN,
                    revents: 0,
                }];
                if !matches!(sys::poll(&mut pfd, 0), Ok(ready) if ready > 0) {
                    continue;
                }
                match read_frame(dial) {
                    Ok(Some(Incoming {
                        frame: Frame::Welcome { .. },
                        ..
                    })) => welcomed += 1,
                    Ok(None) | Err(_) => closed += 1,
                    Ok(Some(other)) => panic!("a dial read {:?} first", other.frame),
                }
                *slot = None;
            }
        }
        let unanswered = server
            .test_unchecked_ever()
            .iter()
            .filter(|why| why.contains("trustd"))
            .count();
        let ended = DialsEnded {
            welcomed,
            closed,
            refused: server.refused_dials,
            unanswered,
        };
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
        ended
    }

    #[test]
    fn a_connection_is_served_while_another_dials_check_runs() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Mutex, PoisonError, mpsc};
        static SCRIPTED: aterm_uds::deadline::Gate =
            aterm_uds::deadline::Gate::new("akk-unit-scripted-serve");
        let dir = std::env::temp_dir().join(format!("akk-unit-{}-serve", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut server = Server::bind(ServeConfig {
            socket: dir.join("k.sock"),
            identity: Identity::same_uid(),
            relaunch: Relauncher::Unavailable,
            version: "test".into(),
            test_drop_custody: false,
        })
        .expect("bind");
        assert_eq!(server.poll_wait(TICK_MS), TICK_MS, "idle");
        server.check_hang = 2 * HANG;
        let (answer, held) = mpsc::channel::<()>();
        let held = Mutex::new(Some(held));
        let first = AtomicBool::new(true);
        server.test_check = Some(Box::new(move || {
            if first.swap(false, Ordering::SeqCst) {
                return PeerCheck::start_on(&SCRIPTED, HANG, || Ok(()));
            }
            let held = held
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
                .expect("two dials");
            PeerCheck::start_on(&SCRIPTED, 2 * HANG, move || {
                held.recv_timeout(2 * HANG)
                    .map_err(|e| CheckError::Unchecked(e.to_string()))
            })
        }));
        let window = CtlStream::connect(server.socket()).expect("the window's dial");
        let began = Instant::now();
        while server.conns.is_empty() {
            assert!(began.elapsed() < HANG, "the window was never admitted");
            server.step(10).expect("turn");
        }
        let dial = CtlStream::connect(server.socket()).expect("the second dial");
        server.step(0).expect("its accept");
        assert_eq!(server.dialing.len(), 1, "its check runs");
        for (asked, waits) in [
            (TICK_MS, CHECK_TICK_MS),
            (-1, CHECK_TICK_MS),
            (0, 0),
            (1, 1),
        ] {
            assert_eq!(server.poll_wait(asked), waits, "asked {asked} ms");
        }
        write_frame(&window, &hello_app(), None).expect("the window's HELLO");
        let welcomed = std::thread::spawn(move || {
            window
                .set_read_timeout(Some(HANG))
                .expect("the hang detector");
            let heard = read_frame(&window).map(|i| i.map(|i| i.frame));
            answer.send(()).expect("the second check waits");
            (window, heard)
        });
        let began = Instant::now();
        while server.conns.len() < 2 {
            assert!(began.elapsed() < HANG, "the second dial was never admitted");
            server.step(10).expect("turn");
        }
        let (window, heard) = welcomed.join().expect("the window's reader");
        assert!(
            matches!(heard, Ok(Some(Frame::Welcome { .. }))),
            "the window's HELLO went unanswered while the check ran: {heard:?}"
        );
        assert_eq!(server.poll_wait(TICK_MS), TICK_MS, "no check runs");
        drop((window, dial, server));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn queued_dials_are_checked_at_once_each_under_its_own_deadline() {
        assert_eq!(
            dials_whose_checks_need_each_other(3, DIAL_CHECKS_MAX, HANG),
            DialsEnded {
                welcomed: 3,
                closed: 0,
                refused: 0,
                unanswered: 0
            }
        );
        let one_at_a_time = dials_whose_checks_need_each_other(3, 1, Duration::from_millis(200));
        let DialsEnded {
            welcomed,
            closed,
            refused,
            unanswered,
        } = one_at_a_time;
        assert!(
            unanswered >= 2
                && closed == unanswered
                && refused == closed as u64
                && welcomed + closed == 3,
            "one check at a time: {one_at_a_time:?}"
        );
    }

    #[test]
    fn a_passed_dial_is_admitted_behind_a_dial_whose_check_still_runs() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Mutex, PoisonError, mpsc};
        static SCRIPTED: aterm_uds::deadline::Gate =
            aterm_uds::deadline::Gate::new("akk-unit-scripted-behind");
        let dir = std::env::temp_dir().join(format!("akk-unit-{}-behind", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut server = Server::bind(ServeConfig {
            socket: dir.join("k.sock"),
            identity: Identity::same_uid(),
            relaunch: Relauncher::Unavailable,
            version: "test".into(),
            test_drop_custody: false,
        })
        .expect("bind");
        server.check_hang = 2 * HANG;
        let (release, held) = mpsc::channel::<()>();
        let held = Mutex::new(Some(held));
        let first = AtomicBool::new(true);
        server.test_check = Some(Box::new(move || {
            if !first.swap(false, Ordering::SeqCst) {
                return PeerCheck::start_on(&SCRIPTED, HANG, || Ok(()));
            }
            let held = held
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
                .expect("one slow dial");
            PeerCheck::start_on(&SCRIPTED, 2 * HANG, move || {
                held.recv_timeout(2 * HANG)
                    .map_err(|e| CheckError::Unchecked(e.to_string()))
            })
        }));
        let slow = CtlStream::connect(server.socket()).expect("the slow dial");
        server.step(0).expect("its accept");
        assert_eq!(server.dialing.len(), 1, "the slow check runs");
        let fast = CtlStream::connect(server.socket()).expect("the fast dial");
        let began = Instant::now();
        while server.conns.is_empty() {
            assert!(
                began.elapsed() < HANG,
                "the fast dial waited behind the slow one's check"
            );
            server.step(10).expect("turn");
        }
        assert_eq!(
            (server.conns.len(), server.dialing.len()),
            (1, 1),
            "the slow dial is still being checked"
        );
        release.send(()).expect("the slow check waits");
        let began = Instant::now();
        while server.conns.len() < 2 {
            assert!(began.elapsed() < HANG, "the slow dial was never admitted");
            server.step(10).expect("turn");
        }
        assert_eq!(server.refused_dials, 0, "none turned away");
        drop((slow, fast, server));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
