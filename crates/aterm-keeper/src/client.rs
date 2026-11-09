// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The window's side of AKP1: [`KeeperClient`] and [`KeeperLink`].

use std::cell::{Cell, RefCell};
use std::io;
use std::os::fd::{AsFd as _, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::time::Duration;

use aterm_uds::CtlStream;

use crate::frameio::{Incoming, read_frame, write_frame};
use crate::identity::{CheckError, Identity, IdentityPolicy};
use crate::wire::{
    CAP_JOURNAL, CAP_LINK, CAP_SENDS_BYE, CAP_SYNC, COMPANION_JOURNAL, Frame, JournalOffer,
    MarkerRef, MasterHeader, PROTO, PeerClass, RefuseCode,
};

/// One OFFER as the window receives it.
#[derive(Debug)]
pub struct Offered {
    pub master: OwnedFd,
    pub header: MasterHeader,
    pub tag: Vec<u8>,
    pub meta: Vec<u8>,
    /// Crash-restore region offered with the master.
    pub companion: Option<OfferedCompanion>,
    /// Keeper poison brake (`JournalOffer::attempts`).
    pub attempts: u8,
}

/// Crash-restore journal region offered by a keeper.
#[derive(Debug)]
pub struct OfferedCompanion {
    pub region: OwnedFd,
    pub kind: u8,
    pub len: u64,
}

/// Error payload when a keeper fails this build's identity check or states a weaker policy.
#[derive(Debug)]
pub struct NotThisBuild(pub String);

impl std::fmt::Display for NotThisBuild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NotThisBuild {}

/// Whether `e` is [`KeeperClient::connect`] refusing a keeper that is not this build's.
#[must_use]
pub fn refused_identity(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<NotThisBuild>())
}

fn check_failed(e: CheckError) -> io::Error {
    match e {
        CheckError::Refused(why) => {
            io::Error::new(io::ErrorKind::PermissionDenied, NotThisBuild(why))
        }
        CheckError::Unchecked(why) | CheckError::Unanswered(why) => io::Error::other(why),
    }
}

/// Minimum wait for a HELLO's answer from the keeper.
#[cfg(not(test))]
pub const WELCOME_WAIT: Duration = aterm_uds::peer::CHECK_HANG.saturating_add(FRAME_BUDGET);
/// Unit tests: the hang detector, since every keeper these tests dial answers a HELLO.
#[cfg(test)]
pub const WELCOME_WAIT: Duration = aterm_spec::wait::MUST_HAPPEN;

#[cfg(not(test))]
const FRAME_BUDGET: Duration = Duration::from_secs(2);

/// One connection to a keeper.
#[derive(Debug)]
pub struct KeeperClient {
    stream: CtlStream,
    /// The read and write timeout [`Self::connect`] set: every frame's but a HELLO's answer,
    /// which waits [`WELCOME_WAIT`] at least ([`Self::answer`]).
    frame_wait: Duration,
    /// The policy this client checked the keeper under, which the keeper's
    /// own must be no weaker than ([`KeeperClient::served`]).
    policy: IdentityPolicy,
    /// The capabilities the keeper's WELCOME stated (0 before it, or from an older keeper):
    /// a window sends companions only to one that states [`CAP_JOURNAL`], and SYNC only to one
    /// that states [`CAP_SYNC`].
    keeper_caps: Cell<u8>,
    /// The number of the last SYNC written on this connection ([`Self::sync`]).
    syncs: Cell<u64>,
    /// The highest SYNC number the keeper wrote back ([`Self::drain`] reads it): every frame
    /// written before that SYNC was read.
    synced: Cell<u64>,
    /// The RELEASEs [`Self::retire`] wrote on this connection whose retirement awaits the
    /// keeper's answer: a [`KeeperLink`] started on this connection owes each one.
    retiring: RefCell<Vec<(u64, Owe)>>,
}

impl KeeperClient {
    /// Dial `path`, check the keeper's identity within `timeout`, and set a read timeout.
    /// # Errors
    /// The dial failing, the keeper failing the identity check (a [`NotThisBuild`] inside `PermissionDenied`), or the check not running (trustd not answering within `timeout` among the reasons).
    pub fn connect(path: &Path, identity: &Identity, timeout: Duration) -> io::Result<Self> {
        let stream = aterm_uds::kept_out(|| CtlStream::connect(path))?;
        identity.check(&stream, timeout).map_err(check_failed)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        Ok(Self {
            stream,
            frame_wait: timeout,
            policy: identity.policy(),
            keeper_caps: Cell::new(0),
            syncs: Cell::new(0),
            synced: Cell::new(0),
            retiring: RefCell::new(Vec::new()),
        })
    }

    /// Whether the keeper's WELCOME stated it takes crash-restore companions.
    #[must_use]
    pub const fn keeper_journals(&self) -> bool {
        self.keeper_caps.get() & CAP_JOURNAL != 0
    }

    /// Whether the keeper's WELCOME stated it answers SYNC.
    #[must_use]
    pub const fn keeper_syncs(&self) -> bool {
        self.keeper_caps.get() & CAP_SYNC != 0
    }

    /// Ask the keeper to confirm when it has read every frame written so far.
    /// # Errors
    /// The keeper did not state SYNC support, the sequence is exhausted, or the write failed.
    pub fn sync(&self) -> io::Result<u64> {
        if !self.keeper_syncs() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the keeper did not state SYNC support",
            ));
        }
        let seq = self
            .syncs
            .get()
            .checked_add(1)
            .ok_or_else(|| io::Error::other("the keeper SYNC sequence is exhausted"))?;
        self.send(&Frame::Sync { seq }, None)?;
        self.syncs.set(seq);
        Ok(seq)
    }

    /// The highest SYNC number the keeper answered on this connection.
    #[must_use]
    pub const fn synced(&self) -> u64 {
        self.synced.get()
    }

    fn welcomed(&self, identity: Option<IdentityPolicy>, caps: Option<u8>) -> io::Result<()> {
        self.served(identity)?;
        self.keeper_caps.set(caps.unwrap_or(0));
        Ok(())
    }

    fn served(&self, served: Option<IdentityPolicy>) -> io::Result<()> {
        match (self.policy, served) {
            (IdentityPolicy::SameUid, _) | (_, Some(IdentityPolicy::Designated)) => Ok(()),
            (IdentityPolicy::Designated, other) => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                NotThisBuild(match other {
                    Some(IdentityPolicy::SameUid) => {
                        "the keeper admits any same-uid peer (--identity uid), not only this \
                         build's code"
                            .to_string()
                    }
                    _ => "the keeper does not say it admits only this build's code (an older \
                          keeper)"
                        .to_string(),
                }),
            )),
        }
    }

    /// Send a frame and optional descriptor.
    /// # Errors
    /// The write failing.
    pub fn send(&self, frame: &Frame, fd: Option<&OwnedFd>) -> io::Result<()> {
        write_frame(&self.stream, frame, fd.map(|f| f.as_fd()))
    }

    /// Read the next frame, returning `None` at EOF.
    /// # Errors
    /// A malformed frame, the timeout, or the socket's errors.
    pub fn recv(&self) -> io::Result<Option<Incoming>> {
        read_frame(&self.stream)
    }

    /// Send `frame` and `read` its answer within [`WELCOME_WAIT`], the keeper's peer check
    /// included, then go back to the frame wait.
    ///
    /// The longer wait is set before `frame` goes out, while the keeper cannot yet have answered:
    /// a keeper may close once it has, and macOS refuses a socket option on a unix socket whose
    /// peer closed (EINVAL), though what the keeper wrote is still there to read. For the same
    /// reason a failed reset does not undo an answer already read; it leaves the longer wait,
    /// still a bound, on the socket.
    fn exchange<T>(
        &self,
        frame: &Frame,
        read: impl FnOnce(&Self) -> io::Result<T>,
    ) -> io::Result<T> {
        self.stream
            .set_read_timeout(Some(WELCOME_WAIT.max(self.frame_wait)))?;
        let answer = self.send(frame, None).and_then(|()| read(self));
        let _reset = self.stream.set_read_timeout(Some(self.frame_wait));
        answer
    }

    /// Send HELLO as a window and collect the WELCOME's offers.
    /// # Errors
    /// The exchange failing or the keeper answering out of protocol.
    pub fn hello_app(&self, build: &str) -> io::Result<Vec<Offered>> {
        self.hello_app_marked(build, None, false)
    }

    /// Send [`Self::hello_app`] naming this window's crash marker.
    /// # Errors
    /// The exchange failing or the keeper answering out of protocol.
    pub fn hello_app_marked(
        &self,
        build: &str,
        marker: Option<MarkerRef>,
        journal: bool,
    ) -> io::Result<Vec<Offered>> {
        self.exchange(
            &Frame::Hello {
                proto: PROTO,
                class: PeerClass::App,
                caps: CAP_SENDS_BYE | if journal { CAP_JOURNAL } else { 0 },
                build: build.to_string(),
                marker,
            },
            |c| c.welcome_offers(journal),
        )
    }

    fn welcome_offers(&self, journal: bool) -> io::Result<Vec<Offered>> {
        let n = match self.recv()?.map(|i| i.frame) {
            Some(Frame::Welcome {
                offers,
                identity,
                caps,
                ..
            }) => {
                self.welcomed(identity, caps)?;
                offers
            }
            other => return Err(protocol(format!("expected WELCOME, got {other:?}"))),
        };
        let mut offers = Vec::with_capacity(usize::from(n));
        for _ in 0..n {
            let (master, header, tag, meta, trail) = match self.recv()? {
                Some(Incoming {
                    frame:
                        Frame::Offer {
                            header,
                            tag,
                            meta,
                            journal: trail,
                        },
                    fd: Some(master),
                }) => (master, header, tag, meta, trail),
                other => return Err(protocol(format!("expected OFFER, got {other:?}"))),
            };
            let JournalOffer {
                attempts,
                companions,
            } = trail.filter(|_| journal).unwrap_or_default();
            let companion = match companions {
                0 => None,
                1 => Some(self.companion_offer(header.rdev)?),
                n => return Err(protocol(format!("an OFFER of {n} companions"))),
            };
            offers.push(Offered {
                master,
                header,
                tag,
                meta,
                companion,
                attempts,
            });
        }
        Ok(offers)
    }

    fn companion_offer(&self, rdev: u64) -> io::Result<OfferedCompanion> {
        match self.recv()? {
            Some(Incoming {
                frame:
                    Frame::CompanionOffer {
                        rdev: of,
                        kind,
                        len,
                    },
                fd: Some(region),
            }) if of == rdev => Ok(OfferedCompanion { region, kind, len }),
            other => Err(protocol(format!(
                "expected the COMPANION_OFFER of rdev {rdev}, got {other:?}"
            ))),
        }
    }

    /// Send HELLO as a window's link ([`CAP_LINK`]).
    /// # Errors
    /// The exchange failing or the keeper answering out of protocol.
    pub fn hello_link(
        &self,
        build: &str,
        marker: Option<MarkerRef>,
        journal: bool,
    ) -> io::Result<()> {
        self.exchange(
            &Frame::Hello {
                proto: PROTO,
                class: PeerClass::App,
                caps: CAP_SENDS_BYE | CAP_LINK | if journal { CAP_JOURNAL } else { 0 },
                build: build.to_string(),
                marker,
            },
            Self::empty_welcome,
        )
    }

    fn empty_welcome(&self) -> io::Result<()> {
        match self.recv()?.map(|i| i.frame) {
            Some(Frame::Welcome {
                offers: 0,
                identity,
                caps,
                ..
            }) => self.welcomed(identity, caps),
            other => Err(protocol(format!(
                "expected an empty WELCOME, got {other:?}"
            ))),
        }
    }

    /// Send HELLO as a status client and ask for the status text.
    /// # Errors
    /// The exchange failing or the keeper answering out of protocol.
    pub fn status(&self, build: &str) -> io::Result<String> {
        self.exchange(
            &Frame::Hello {
                proto: PROTO,
                class: PeerClass::Status,
                caps: 0,
                build: build.to_string(),
                marker: None,
            },
            Self::empty_welcome,
        )?;
        self.exchange(
            &Frame::Status {
                text: String::new(),
            },
            |c| match c.recv()?.map(|i| i.frame) {
                Some(Frame::Status { text }) => Ok(text),
                other => Err(protocol(format!("expected STATUS, got {other:?}"))),
            },
        )
    }

    /// Read every frame already waiting, keeping only a SYNC answer; returns `false` once the keeper is gone.
    #[must_use]
    pub fn drain(&self) -> bool {
        self.drain_with_refusals(|_, _, _| {})
    }

    /// [`Self::drain`], telling `refused` each REFUSED it reads: its rdev, its code, and the
    /// highest SYNC answered before it.
    fn drain_with_refusals(&self, mut refused: impl FnMut(u64, RefuseCode, u64)) -> bool {
        use std::os::fd::AsRawFd as _;
        loop {
            let mut fds = [crate::sys::PollFd {
                fd: self.stream.as_raw_fd(),
                events: crate::sys::POLLIN,
                revents: 0,
            }];
            match crate::sys::poll(&mut fds, 0) {
                Ok(0) => return true,
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return false,
            }
            match self.recv() {
                Ok(Some(Incoming {
                    frame: Frame::Sync { seq },
                    ..
                })) => self
                    .synced
                    .set(self.synced.get().max(seq.min(self.syncs.get()))),
                Ok(Some(Incoming {
                    frame: Frame::Refused { rdev, code },
                    ..
                })) => refused(rdev, code, self.synced.get()),
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => return false,
            }
        }
    }

    /// Register a master duplicate with the keeper.
    /// # Errors
    /// The write failing.
    pub fn register(&self, master: &OwnedFd, header: MasterHeader, tag: Vec<u8>) -> io::Result<()> {
        self.send(&Frame::Register { header, tag }, Some(master))
    }

    /// Send the read-only custody descriptor of `rdev`'s journal region.
    /// # Errors
    /// The write failing.
    pub fn companion(&self, rdev: u64, region: &OwnedFd, len: u64) -> io::Result<()> {
        self.send(
            &Frame::Companion {
                rdev,
                kind: COMPANION_JOURNAL,
                len,
            },
            Some(region),
        )
    }

    /// Tell the keeper that this connection's claim on `rdev` has no journal region.
    /// # Errors
    /// The write failing.
    pub fn companion_release(&self, rdev: u64) -> io::Result<()> {
        self.send(
            &Frame::CompanionRelease {
                rdev,
                kind: COMPANION_JOURNAL,
            },
            None,
        )
    }

    /// RELEASE `rdev`, a master this connection was offered and the window will not keep, with
    /// a SYNC right behind it, and report the retirement of the journal the offer carried to
    /// `observer` once it is confirmed: the keeper answers that SYNC with no refusal of the
    /// RELEASE before it, or refuses it as naming no record (`NoSuchRecord`: the journal went
    /// with its record). A [`KeeperLink`] started on this connection owes the retirement
    /// until then, and tells it again to the next connection if this one ends first. To a keeper
    /// whose WELCOME stated no [`CAP_SYNC`] the write alone confirms it ([`Owe::written_unsynced`]).
    /// # Errors
    /// The RELEASE's write failing; the retirement is still owed to the next connection.
    pub fn retire(&self, rdev: u64, observer: std::sync::Arc<ReadObserver>) -> io::Result<()> {
        let written = self.send(&Frame::Release { rdev }, None);
        let mut owe = Owe::retiring(observer);
        if written.is_ok() {
            if !self.keeper_syncs() {
                owe.written_unsynced(std::time::Instant::now());
                return Ok(());
            }
            owe.sync = self.sync().ok().map(|n| (n, std::time::Instant::now()));
        }
        self.retiring.borrow_mut().push((rdev, owe));
        written
    }
}

fn protocol(why: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

/// The first read the keeper confirmed of a frame reporting to a [`ReadObserver`], on this
/// process's monotonic clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfirmedRead {
    /// When the keeper was asked: the SYNC written behind the frame that settled the journal (a
    /// region's COMPANION, a COMPANION_RELEASE, or the master's RELEASE). A confirmation that
    /// waited on no SYNC (a RELEASE written to a keeper without [`CAP_SYNC`], or one a REGISTER at
    /// its rdev confirmed before a SYNC behind it was written) was asked and answered at once,
    /// so this is [`Self::answer_read`].
    pub sync_sent: std::time::Instant,
    /// When the link confirmed it: its worker read the keeper's answer to that SYNC, or the
    /// REFUSED (`NoSuchRecord`) that confirmed a retirement, or took the REGISTER at its rdev,
    /// or, to a keeper without [`CAP_SYNC`], wrote the RELEASE.
    pub answer_read: std::time::Instant,
}

/// A process-local admission's view of one old journal authority: whether the keeper confirmed
/// that it can no longer offer that journal, by reading a frame that reports here: a new region
/// or a COMPANION_RELEASE for the master that carried it
/// ([`CompanionReceipt::reporting_confirmation`]), or the master's RELEASE
/// ([`KeeperClient::retire`], [`LinkOp::Retire`], or a [`LinkOp::Release`] of a master whose
/// last such frame the keeper had not yet confirmed); and when the first such read was asked
/// for and answered ([`ConfirmedRead`]). A frame that reports here and is superseded before its
/// confirmation (the master's next COMPANION or COMPANION_RELEASE, or its RELEASE) hands this
/// on to the frame that superseded it, whose read settles the old journal as well.
#[derive(Debug, Default)]
pub struct ReadObserver {
    read: std::sync::OnceLock<ConfirmedRead>,
}

impl ReadObserver {
    /// Whether a frame reporting here was confirmed read.
    #[must_use]
    pub fn confirmed(&self) -> bool {
        self.read.get().is_some()
    }

    /// The first confirmed read, `None` before it.
    #[must_use]
    pub fn confirmation(&self) -> Option<ConfirmedRead> {
        self.read.get().copied()
    }

    fn note(&self, read: ConfirmedRead) {
        let _ = self.read.set(read);
    }
}

/// A region (or, for a COMPANION_RELEASE, its absence) reached the keeper only after a SYNC behind
/// its COMPANION or COMPANION_RELEASE was answered.
#[derive(Debug, Default)]
pub struct CompanionReceipt {
    confirmed: std::sync::atomic::AtomicBool,
    observer: Option<std::sync::Arc<ReadObserver>>,
}

impl CompanionReceipt {
    /// Also publish this exact accepted read to a process-local admission observer.
    #[must_use]
    pub const fn reporting_confirmation(observer: std::sync::Arc<ReadObserver>) -> Self {
        Self {
            confirmed: std::sync::atomic::AtomicBool::new(false),
            observer: Some(observer),
        }
    }

    /// Whether the keeper confirmed reading this replacement.
    #[must_use]
    pub fn confirmed(&self) -> bool {
        self.confirmed.load(std::sync::atomic::Ordering::Acquire)
    }

    fn confirm(&self, sync_sent: std::time::Instant) {
        self.confirmed
            .store(true, std::sync::atomic::Ordering::Release);
        if let Some(observer) = &self.observer {
            observer.note(ConfirmedRead {
                sync_sent,
                answer_read: std::time::Instant::now(),
            });
        }
    }

    /// A captured keeper read, for in-memory queue fixtures only.
    #[cfg(any(test, debug_assertions))]
    pub fn confirm_captured(&self) {
        self.confirm(std::time::Instant::now());
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CompanionFence {
    after: u64,
    until: u64,
    /// When the SYNC numbered `until` was written.
    sent: std::time::Instant,
    refused: bool,
}

#[derive(Debug)]
struct CompanionRead {
    receipt: std::sync::Arc<CompanionReceipt>,
    /// The observers of the master's earlier frames this one superseded before the keeper
    /// confirmed them ([`superseding`]): this frame's confirmation is theirs too.
    carried: Vec<std::sync::Arc<ReadObserver>>,
    fence: Option<CompanionFence>,
}

impl CompanionRead {
    /// The read of a frame the window just queued for a held master, with its own `receipt`, if
    /// any. It takes on the observers of `was`, the read the master's last frame waited on, when
    /// the keeper had not yet confirmed that one: the keeper reads frames in order, and its read
    /// of this one settles the master's region (a new one, or none), so it can no longer offer
    /// the old journal those observers wait on either.
    fn superseding(
        receipt: Option<std::sync::Arc<CompanionReceipt>>,
        was: Option<Self>,
    ) -> Option<Self> {
        let mut carried = was.map(Self::observers).unwrap_or_default();
        if let Some(own) = receipt.as_ref().and_then(|r| r.observer.as_ref()) {
            carried.retain(|o| !std::sync::Arc::ptr_eq(o, own));
        }
        if receipt.is_none() && carried.is_empty() {
            return None;
        }
        Some(Self {
            receipt: receipt.unwrap_or_default(),
            carried,
            fence: None,
        })
    }

    /// Every observer its confirmation reports to.
    fn observers(self) -> Vec<std::sync::Arc<ReadObserver>> {
        let mut all = self.carried;
        if let Some(own) = &self.receipt.observer
            && !all.iter().any(|o| std::sync::Arc::ptr_eq(o, own))
        {
            all.push(std::sync::Arc::clone(own));
        }
        all
    }
}

const fn note_companion_refusal(read: &mut Option<CompanionRead>, synced: u64) {
    if let Some(CompanionRead {
        fence: Some(fence), ..
    }) = read
        && synced >= fence.after
        && synced < fence.until
    {
        fence.refused = true;
    }
}

fn confirm_companion(read: &mut Option<CompanionRead>, synced: u64) {
    if let Some(fence) = read
        .as_ref()
        .and_then(|r| r.fence)
        .filter(|f| !f.refused && f.until <= synced)
        && let Some(read) = read.take()
    {
        read.receipt.confirm(fence.sent);
        let answer_read = std::time::Instant::now();
        for observer in &read.carried {
            observer.note(ConfirmedRead {
                sync_sent: fence.sent,
                answer_read,
            });
        }
    }
}

/// One message to the keeper, queued by the window.
#[derive(Debug)]
pub enum LinkOp {
    /// Register a fresh or adopted master duplicate.
    Register {
        master: OwnedFd,
        header: MasterHeader,
        tag: Vec<u8>,
    },
    /// The newest META of a master.
    Meta { rdev: u64, meta: Vec<u8> },
    /// A tab or pane closed. When the master's last COMPANION or COMPANION_RELEASE reports to a
    /// [`ReadObserver`] and the keeper had not confirmed it, the RELEASE is a retirement of the
    /// old journal that observer waits on, as [`Self::Retire`] is: its read closes the record,
    /// and the journal with it.
    Release { rdev: u64 },
    /// RELEASE a master the window was offered and will not keep (an adoption it never placed),
    /// owed until the keeper confirms it, and then report the retirement of the journal its
    /// offer carried to `observer`: as [`KeeperClient::retire`], whose rules this follows.
    Retire {
        rdev: u64,
        observer: std::sync::Arc<ReadObserver>,
    },
    /// An update's successor, before its grant.
    Pending { pid: u32 },
    /// The quit, written on the final-exit path just before `exit(0)`.
    Bye,
    /// Attach the journal region of a master already queued for registration.
    Companion {
        rdev: u64,
        region: OwnedFd,
        len: u64,
        /// Optional read confirmation for a region that replaces a live predecessor.
        receipt: Option<std::sync::Arc<CompanionReceipt>>,
    },
    /// Release any journal region held for `rdev`.
    CompanionRelease {
        rdev: u64,
        /// Optional read confirmation: the keeper read that the claim has no region, so it
        /// dropped the one an offer of the master carried (an adopted session with no journal).
        receipt: Option<std::sync::Arc<CompanionReceipt>>,
    },
}

/// Queue bound for ordinary ops.
pub const LINK_QUEUE: usize = 256;

/// Extra channel capacity for waited ops, which ordinary offers cannot fill.
pub const WAITED_ROOM: usize = 4;

/// How many ordinary ops the queue holds that its worker has not taken: never more than
/// [`LINK_QUEUE`], so the [`WAITED_ROOM`] places past them stay free for waited ops.
#[derive(Debug, Default)]
struct Room(std::sync::atomic::AtomicUsize);

impl Room {
    fn claim(&self) -> bool {
        self.0
            .try_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |held| (held < LINK_QUEUE).then_some(held + 1),
            )
            .is_ok()
    }

    fn give_back(&self, n: usize) {
        let _ = self.0.try_update(
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
            |held| Some(held.saturating_sub(n)),
        );
    }

    fn clear(&self) {
        self.0.store(0, std::sync::atomic::Ordering::Release);
    }
}

struct Ended(std::sync::Arc<Room>);

impl Drop for Ended {
    fn drop(&mut self) {
        self.0.clear();
    }
}

/// Why [`KeeperLink::offer`] queued nothing, with the op it hands back.
#[derive(Debug)]
pub enum Refused {
    /// The queue is full.
    Full(LinkOp),
    /// The worker has ended.
    Gone(LinkOp),
}

/// The far end of a [`KeeperLink::stalled`] link.
#[cfg(any(test, debug_assertions))]
#[derive(Debug)]
pub struct Stalled {
    /// Behind a lock, so the stand-in can be shared as the worker it stands for is not.
    rx: std::sync::Mutex<Receiver<Queued>>,
    /// The link's room, which the ordinary ops taken give back as a worker's do.
    room: std::sync::Arc<Room>,
    /// The bell's far end, which nothing answers: the link rings once, then finds it rung.
    _door: Door,
}

#[cfg(any(test, debug_assertions))]
impl Stalled {
    /// Take every op queued so far, in order.
    #[must_use]
    pub fn drain(&self) -> Vec<LinkOp> {
        self.drain_at(std::time::Instant::now())
    }

    fn drain_at(&self, now: std::time::Instant) -> Vec<LinkOp> {
        let taken: Vec<Queued> = self
            .rx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .try_iter()
            .collect();
        self.room
            .give_back(taken.iter().filter(|(_, ack)| ack.is_none()).count());
        taken
            .into_iter()
            .filter_map(|queued| claim_queued(queued, now))
            .map(|(op, _)| op)
            .collect()
    }
}

#[cfg(any(test, debug_assertions))]
impl Drop for Stalled {
    fn drop(&mut self) {
        self.room.clear();
    }
}

type Queued = (LinkOp, Option<Waited>);

/// Cancellation belongs to one queued wait, never to the link or another op.
#[derive(Debug)]
struct Waited {
    answer: SyncSender<bool>,
    state: std::sync::Arc<std::sync::atomic::AtomicU8>,
    until: Option<std::time::Instant>,
}

impl Waited {
    const QUEUED: u8 = 0;
    const CLAIMED: u8 = 1;
    const CANCELLED: u8 = 2;

    fn cancel(state: &std::sync::atomic::AtomicU8) {
        let _ = state.compare_exchange(
            Self::QUEUED,
            Self::CANCELLED,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        );
    }

    fn claim_at(&self, now: std::time::Instant) -> bool {
        if self.until.is_some_and(|until| now >= until) {
            Self::cancel(&self.state);
            return false;
        }
        self.state
            .compare_exchange(
                Self::QUEUED,
                Self::CLAIMED,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok()
    }

    fn try_send(&self, answer: bool) -> Result<(), mpsc::TrySendError<bool>> {
        self.answer.try_send(answer)
    }
}

fn claim_queued(queued: Queued, now: std::time::Instant) -> Option<Queued> {
    if let Some(waited) = &queued.1
        && !waited.claim_at(now)
    {
        let _ = waited.try_send(false);
        return None;
    }
    Some(queued)
}

/// Worker thread that owns the keeper connection and reconnects after a keeper restart.
#[derive(Debug)]
pub struct KeeperLink {
    /// Bounded at [`LINK_QUEUE`] ordinary ops plus [`WAITED_ROOM`] waited ones.
    tx: SyncSender<Queued>,
    /// The ordinary ops' share of the queue ([`Self::offer`]).
    room: std::sync::Arc<Room>,
    /// Rung behind every op queued, so the worker, waiting on its [`Door`] and the keeper's
    /// socket together, wakes for either as it arrives.
    bell: Bell,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// The queue's doorbell, rung by [`KeeperLink`] once it has queued an op.
#[derive(Debug)]
struct Bell {
    /// Its end of a socket pair whose other end is the worker's [`Door`]; non-blocking.
    near: CtlStream,
    /// Set from a ring until the worker answers it, so a burst of ops writes one byte.
    rung: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// The worker's end of the [`Bell`], waited on beside the keeper's socket ([`Door::next`]).
#[derive(Debug)]
struct Door {
    /// Non-blocking.
    far: CtlStream,
    rung: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The bell's end has closed: the [`KeeperLink`] is gone, so nothing will ring again.
    hung_up: bool,
}

/// A bell and its door: a socket pair, each end non-blocking and kept out of every child.
fn doorbell() -> io::Result<(Bell, Door)> {
    let (near, far) = aterm_uds::kept_out(CtlStream::pair)?;
    for end in [&near, &far] {
        end.set_nonblocking(true)?;
    }
    let rung = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    Ok((
        Bell {
            near,
            rung: std::sync::Arc::clone(&rung),
        },
        Door {
            far,
            rung,
            hung_up: false,
        },
    ))
}

impl Bell {
    /// Tell the worker an op is queued (call it after the op is in the channel).
    fn ring(&self) {
        self.ring_with(|| aterm_uds::fdpass::send_vectored(&self.near, &[io::IoSlice::new(&[1])]));
    }

    /// Coalesce rings, retrying an interrupted write before leaving the worker to wait.
    fn ring_with(&self, mut write: impl FnMut() -> io::Result<usize>) {
        if self.rung.swap(true, std::sync::atomic::Ordering::AcqRel) {
            // An earlier ring is unanswered: its byte is still to be read, and the worker
            // clears the flag before it reads the queue.
            return;
        }
        loop {
            match write() {
                Ok(n) if n > 0 => return,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                // A full door already holds bytes that wake the worker.
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
                // No byte was written: allow a later ring to try again.
                Ok(_) | Err(_) => {
                    self.rung.store(false, std::sync::atomic::Ordering::Release);
                    return;
                }
            }
        }
    }
}

impl Door {
    /// Answer every ring so far: clear the flag, then read the bytes written.
    fn answer(&mut self) {
        use std::io::Read as _;
        // A swap, not a store: reading the flag a ring set acquires that ring's op.
        self.rung.swap(false, std::sync::atomic::Ordering::AcqRel);
        let mut bytes = [0u8; 64];
        loop {
            match (&self.far).read(&mut bytes) {
                Ok(0) => {
                    self.hung_up = true;
                    return;
                }
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
    }

    /// What wakes the worker next: the op queued at once when there is one, else as soon as one
    /// is; [`Woke::Keeper`] once `keeper` (the link's connection) has something to read with no
    /// op queued; [`Woke::Idle`] once `idle` has passed with neither; [`Woke::Closed`] once the
    /// queue is closed and empty. A ring whose op was already taken, or a signal, wakes nobody:
    /// the wait goes on for what is left of `idle`.
    fn next(
        &mut self,
        rx: &Receiver<Queued>,
        keeper: Option<&KeeperClient>,
        idle: Duration,
    ) -> Woke {
        use std::os::fd::AsRawFd as _;
        let started = std::time::Instant::now();
        loop {
            self.answer();
            match rx.try_recv() {
                Ok(queued) => return Woke::Op(queued),
                Err(mpsc::TryRecvError::Disconnected) => return Woke::Closed,
                Err(mpsc::TryRecvError::Empty) => {}
            }
            let left = idle.saturating_sub(started.elapsed());
            if self.hung_up {
                // Nothing will ring again, and a hung-up door reads ready for good: wait on the
                // queue alone (the link is gone, so it is closed, or soon will be).
                return Woke::of(rx.recv_timeout(left));
            }
            let mut fds = [crate::sys::PollFd {
                fd: self.far.as_raw_fd(),
                events: crate::sys::POLLIN,
                revents: 0,
            }; 2];
            let watched = match keeper {
                Some(c) => {
                    fds[1].fd = c.stream.as_raw_fd();
                    2
                }
                None => 1,
            };
            // Rounded up, so a wait shorter than a millisecond is not a poll that returns at once.
            let millis = i32::try_from(left.as_micros().div_ceil(1_000)).unwrap_or(i32::MAX);
            let (heard, over) = match crate::sys::poll(&mut fds[..watched], millis) {
                Ok(0) => (false, true),
                Ok(_) => (watched == 2 && fds[1].revents != 0, false),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => (false, false),
                // A poll that fails would fail again at once: wait on the queue alone.
                Err(_) => return Woke::of(rx.recv_timeout(left)),
            };
            if !heard && !over && left > Duration::ZERO {
                continue;
            }
            self.answer();
            return match rx.try_recv() {
                Ok(queued) => Woke::Op(queued),
                Err(mpsc::TryRecvError::Disconnected) => Woke::Closed,
                Err(mpsc::TryRecvError::Empty) if heard => Woke::Keeper,
                Err(mpsc::TryRecvError::Empty) => Woke::Idle,
            };
        }
    }
}

/// What woke the link's worker ([`Door::next`]).
enum Woke {
    /// An op was queued.
    Op(Queued),
    /// The keeper wrote a frame or closed the connection, and no op is queued.
    Keeper,
    /// The idle wait passed with no op queued and nothing from the keeper.
    Idle,
    /// The queue is closed and empty: the link is shut down.
    Closed,
}

impl Woke {
    /// What a wait on the queue alone answered.
    fn of(waited: Result<Queued, mpsc::RecvTimeoutError>) -> Self {
        match waited {
            Ok(queued) => Self::Op(queued),
            Err(mpsc::RecvTimeoutError::Timeout) => Self::Idle,
            Err(mpsc::RecvTimeoutError::Disconnected) => Self::Closed,
        }
    }
}

/// Who a link is: the window it speaks for, as each of its HELLOs says.
#[derive(Debug)]
struct LinkPeer {
    path: PathBuf,
    identity: Identity,
    build: String,
    /// This window's crash marker, named in every link HELLO.
    marker: Option<MarkerRef>,
    /// This window journals its sessions: every link HELLO states [`CAP_JOURNAL`], and its
    /// companions are told to a keeper that states it.
    journal: bool,
    /// How long the worker waits with no op queued and nothing from the keeper before it looks
    /// again ([`LINK_POLL`] but for [`KeeperLink::start_idling`]): what paces its tries at a
    /// keeper it lost.
    idle: Duration,
}

impl KeeperLink {
    /// Start the worker for the keeper at `path`.
    /// # Errors
    /// The worker's doorbell (a socket pair) not being made, or the thread failing to spawn.
    pub fn start(path: PathBuf, identity: Identity, build: String) -> io::Result<Self> {
        Self::start_on(path, identity, build, None)
    }

    /// Start the worker naming this window's crash marker in every link HELLO.
    /// # Errors
    /// The worker's doorbell (a socket pair) not being made, or the thread failing to spawn.
    pub fn start_marked(
        path: PathBuf,
        identity: Identity,
        build: String,
        marker: Option<MarkerRef>,
        booted: Option<KeeperClient>,
        journal: bool,
    ) -> io::Result<Self> {
        Self::start_idling(path, identity, build, marker, booted, journal, LINK_POLL)
    }

    /// [`Self::start_marked`] with the worker waiting `idle`, not [`LINK_POLL`], when no op is
    /// queued and the keeper wrote nothing: what paces its tries at a keeper it lost.
    /// # Errors
    /// The worker's doorbell (a socket pair) not being made, or the thread failing to spawn.
    pub fn start_idling(
        path: PathBuf,
        identity: Identity,
        build: String,
        marker: Option<MarkerRef>,
        booted: Option<KeeperClient>,
        journal: bool,
        idle: Duration,
    ) -> io::Result<Self> {
        let (tx, rx) = mpsc::sync_channel(LINK_QUEUE + WAITED_ROOM);
        let room = std::sync::Arc::new(Room::default());
        let ended = Ended(std::sync::Arc::clone(&room));
        let (bell, door) = doorbell()?;
        let peer = LinkPeer {
            path,
            identity,
            build,
            marker,
            journal,
            idle,
        };
        let thread = std::thread::Builder::new()
            .name("aterm-keeper-link".to_string())
            .spawn(move || {
                let ended = ended;
                link_worker(&peer, rx, door, booted, &ended.0);
            })?;
        Ok(Self {
            tx,
            room,
            bell,
            thread: Some(thread),
        })
    }

    /// Start the worker on a connection that already sent the window's boot HELLO.
    /// # Errors
    /// The worker's doorbell (a socket pair) not being made, or the thread failing to spawn.
    pub fn start_on(
        path: PathBuf,
        identity: Identity,
        build: String,
        booted: Option<KeeperClient>,
    ) -> io::Result<Self> {
        Self::start_marked(path, identity, build, None, booted, false)
    }

    /// Queue `op`, returning `false` when the queue is full or the worker is gone.
    pub fn enqueue(&self, op: LinkOp) -> bool {
        self.offer(op).is_ok()
    }

    /// Queue `op`, or return it with the reason it was refused.
    /// # Errors
    /// [`Refused::Full`] while the queue holds [`LINK_QUEUE`] ordinary ops, or [`Refused::Gone`] once the worker has ended.
    pub fn offer(&self, op: LinkOp) -> Result<(), Refused> {
        if !self.room.claim() {
            return Err(Refused::Full(op));
        }
        match self.tx.try_send((op, None)) {
            Ok(()) => {
                self.bell.ring();
                Ok(())
            }
            Err(refused) => {
                self.room.give_back(1);
                Err(match refused {
                    TrySendError::Full((op, _)) => Refused::Full(op),
                    TrySendError::Disconnected((op, _)) => Refused::Gone(op),
                })
            }
        }
    }

    /// Create a link with no worker whose queued ops stay until [`Stalled::drain`] takes them.
    /// # Panics
    /// The doorbell's socket pair not being made (out of descriptors).
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn stalled() -> (Self, Stalled) {
        let (tx, rx) = mpsc::sync_channel(LINK_QUEUE + WAITED_ROOM);
        let room = std::sync::Arc::new(Room::default());
        let (bell, door) = doorbell().expect("a stalled link's doorbell");
        (
            Self {
                tx,
                room: std::sync::Arc::clone(&room),
                bell,
                thread: None,
            },
            Stalled {
                rx: std::sync::Mutex::new(rx),
                room,
                _door: door,
            },
        )
    }

    /// Queue `op` using waited room and wait for a written answer.
    pub fn send_and_wait(&self, op: LinkOp, within: Duration) -> bool {
        let lasting = matches!(op, LinkOp::Bye);
        self.send_waited(op, within, lasting)
    }

    /// [`Self::send_and_wait`], but a wait that runs out gives up only the wait, as a BYE's does:
    /// `op` stays queued and is written in its turn, so it keeps its place among the ops queued
    /// before and after it whether or not the caller saw it written.
    pub fn send_lasting_and_wait(&self, op: LinkOp, within: Duration) -> bool {
        self.send_waited(op, within, true)
    }

    /// Queue `op` using waited room and wait at most `within` for a written answer; a `lasting`
    /// op outlives its wait, any other is dropped unwritten once its wait has run out.
    fn send_waited(&self, op: LinkOp, within: Duration, lasting: bool) -> bool {
        let now = std::time::Instant::now();
        let Some(until) = now.checked_add(within) else {
            return false;
        };
        if !lasting && within.is_zero() {
            return false;
        }
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        let state = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(Waited::QUEUED));
        let waited = Waited {
            answer: ack_tx,
            state: std::sync::Arc::clone(&state),
            until: (!lasting).then_some(until),
        };
        if self.tx.try_send((op, Some(waited))).is_err() {
            return false;
        }
        self.bell.ring();
        match ack_rx.recv_timeout(until.saturating_duration_since(std::time::Instant::now())) {
            Ok(written) => written,
            Err(_) => {
                if !lasting {
                    Waited::cancel(&state);
                }
                false
            }
        }
    }

    /// Close the queue and wait for the worker to drain it.
    pub fn shutdown(mut self) {
        let (tx, _) = mpsc::sync_channel(1);
        drop(std::mem::replace(&mut self.tx, tx));
        // Wake the worker to find the queue closed.
        self.bell.ring();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// What the link keeps of a master it registered, by rdev: the duplicate a restarted keeper is
/// registered with again, and where the master's journal stands.
#[derive(Debug)]
struct HeldMaster {
    master: OwnedFd,
    header: MasterHeader,
    tag: Vec<u8>,
    journal: HeldJournal,
}

/// Where a held master's crash-restore journal stands, as the keeper is told it (and told again
/// after a reconnect, behind the master's REGISTER).
#[derive(Debug)]
enum HeldJournal {
    /// Nothing said yet: the window's COMPANION or COMPANION_RELEASE is still queued. A
    /// reconnect says nothing for it, so the keeper's poison window stays open until the window
    /// itself settles the claim (a premature "no journal" would close the region a recovering
    /// window was offered, before it had a region of its own).
    Unsaid,
    /// The read-only custody descriptor of its region, and the region's length.
    Region {
        region: OwnedFd,
        len: u64,
        read: Option<CompanionRead>,
    },
    /// It has no region.
    Released { read: Option<CompanionRead> },
}

impl HeldJournal {
    /// The read its last frame waits on, taken: the keeper has not yet confirmed it.
    fn take_read(&mut self) -> Option<CompanionRead> {
        match self {
            Self::Region { read, .. } | Self::Released { read } => read.take(),
            Self::Unsaid => None,
        }
    }
}

type Held = std::collections::BTreeMap<u64, HeldMaster>;

/// A RELEASE the link owes the keeper (link-worker-delivery (b)).
#[derive(Debug)]
struct Owe {
    /// The SYNC written right behind it on the current connection, and when it was written;
    /// `None` until one is.
    sync: Option<(u64, std::time::Instant)>,
    /// The admission observers its retirement reports to once the keeper confirms it
    /// ([`LinkOp::Retire`], [`KeeperClient::retire`], and a tab's RELEASE whose master's last
    /// journal frame was still unconfirmed, [`Self::releasing`]); none for any other RELEASE.
    retires: Vec<std::sync::Arc<ReadObserver>>,
}

impl Owe {
    fn retiring(observer: std::sync::Arc<ReadObserver>) -> Self {
        Self {
            sync: None,
            retires: vec![observer],
        }
    }

    /// The RELEASE of `held`, a master the link registered, as a tab's close owes it: a
    /// retirement of whatever old journal the read its last frame waits on reports to, since the
    /// keeper's read of the RELEASE closes the record and that journal with it.
    fn releasing(held: HeldMaster) -> Self {
        let mut journal = held.journal;
        Self {
            sync: None,
            retires: journal
                .take_read()
                .map(CompanionRead::observers)
                .unwrap_or_default(),
        }
    }

    /// The keeper confirmed the RELEASE `now`: report the retirement to its observers.
    fn confirm(&self, now: std::time::Instant) {
        for observer in &self.retires {
            observer.note(ConfirmedRead {
                sync_sent: self.sync.map_or(now, |(_, sent)| sent),
                answer_read: now,
            });
        }
    }

    /// The RELEASE was written `now` to a keeper whose WELCOME stated no [`CAP_SYNC`]: to it a
    /// written RELEASE is done (link-worker-delivery (b)), so the write confirms the retirement.
    /// This is the one rule under which a write alone confirms anything; a COMPANION or
    /// COMPANION_RELEASE written to such a keeper leaves its receipt pending ([`send_fenced`]).
    fn written_unsynced(&self, now: std::time::Instant) {
        self.confirm(now);
    }
}

/// The RELEASEs owed, by rdev.
type Owed = std::collections::BTreeMap<u64, Owe>;

enum Send {
    Register(u64),
    Companion(u64),
    CompanionRelease(u64),
    Frame(Frame),
}

/// How long an idle link's worker waits, with no op queued and nothing from the keeper to read,
/// before it looks again: what paces its tries at a keeper it lost. A queued op rings its door and
/// a keeper's frame (a SYNC's answer, a refusal, the keeper's close) makes its socket readable,
/// and either wakes it at once ([`Door::next`]). Only an op's wake or this one dials a keeper,
/// never the wake a keeper's close made.
pub const LINK_POLL: Duration = Duration::from_millis(500);

fn link_worker(
    peer: &LinkPeer,
    rx: Receiver<Queued>,
    mut door: Door,
    booted: Option<KeeperClient>,
    room: &Room,
) {
    let mut held: Held = std::collections::BTreeMap::new();
    let mut metas: std::collections::BTreeMap<u64, Vec<u8>> = std::collections::BTreeMap::new();
    let mut owed: Owed = Owed::new();
    // The boot connection's retirements ([`KeeperClient::retire`]) are this link's to see through.
    for (rdev, owe) in booted.iter().flat_map(|c| c.retiring.take()) {
        keep(&mut owed, rdev, owe);
    }
    let mut client: Option<KeeperClient> = booted;
    let mut backoff = Duration::from_millis(50);
    let mut next_try = std::time::Instant::now();
    loop {
        // A wake only the keeper caused (its answer, its refusal, its close) dials nobody: a
        // keeper that closed the link is tried again on the next op or idle wake, as one the
        // worker found gone there would be, so one that drops every link it welcomes is not
        // dialled again the moment it does.
        let (op, ack, heard) = match door.next(&rx, client.as_ref(), peer.idle) {
            Woke::Op(queued) => {
                if queued.1.is_none() {
                    room.give_back(1);
                }
                match claim_queued(queued, std::time::Instant::now()) {
                    Some((op, ack)) => (Some(op), ack, false),
                    None => continue,
                }
            }
            Woke::Keeper => (None, None, true),
            Woke::Idle => (None, None, false),
            Woke::Closed => return,
        };
        if let Some(c) = &client {
            let open = c.drain_with_refusals(|rdev, code, synced_before| {
                if let Some(HeldMaster {
                    journal: HeldJournal::Region { read, .. } | HeldJournal::Released { read },
                    ..
                }) = held.get_mut(&rdev)
                {
                    note_companion_refusal(read, synced_before);
                }
                refuse_retirement(&mut owed, rdev, code, synced_before);
            });
            settle_owed(&mut owed, c.synced());
            for h in held.values_mut() {
                if let HeldJournal::Region { read, .. } | HeldJournal::Released { read } =
                    &mut h.journal
                {
                    confirm_companion(read, c.synced());
                }
            }
            if !open {
                client = None;
                unconfirm(&mut owed);
                unconfirm_companions(&mut held);
            }
        }
        let bye = matches!(op, Some(LinkOp::Bye));
        let mut must_reach: Option<(u64, Owe)> = None;
        let send = op.and_then(|op| match op {
            LinkOp::Register {
                master,
                header,
                tag,
            } => {
                if let Some(owe) = owed.remove(&header.rdev) {
                    // A new master at the rdev of an owed RELEASE: the kernel gives a pty's
                    // device to another only once every descriptor of the old master is closed,
                    // the keeper's custody copy among them, so the old record, and any journal
                    // it carried, is gone: what a retirement waited to hear.
                    owe.confirm(std::time::Instant::now());
                }
                held.insert(
                    header.rdev,
                    HeldMaster {
                        master,
                        header,
                        tag,
                        journal: HeldJournal::Unsaid,
                    },
                );
                Some(Send::Register(header.rdev))
            }
            LinkOp::Release { rdev } => {
                metas.remove(&rdev);
                let registered = held.remove(&rdev);
                let send = registered.is_some() || client.is_some();
                if let Some(h) = registered {
                    must_reach = Some((rdev, Owe::releasing(h)));
                }
                send.then_some(Send::Frame(Frame::Release { rdev }))
            }
            LinkOp::Retire { rdev, observer } => {
                metas.remove(&rdev);
                let mut owe = Owe::retiring(observer);
                if let Some(h) = held.remove(&rdev) {
                    owe.retires.extend(Owe::releasing(h).retires);
                }
                must_reach = Some((rdev, owe));
                Some(Send::Frame(Frame::Release { rdev }))
            }
            LinkOp::Meta { rdev, meta } => {
                if !held.contains_key(&rdev) {
                    return None;
                }
                metas.insert(rdev, meta.clone());
                Some(Send::Frame(Frame::Meta { rdev, meta }))
            }
            LinkOp::Pending { pid } => Some(Send::Frame(Frame::Pending { pid })),
            LinkOp::Bye => Some(Send::Frame(Frame::Bye)),
            LinkOp::Companion {
                rdev,
                region,
                len,
                receipt,
            } => {
                let entry = held.get_mut(&rdev)?;
                let read = CompanionRead::superseding(receipt, entry.journal.take_read());
                entry.journal = HeldJournal::Region { region, len, read };
                peer.journal.then_some(Send::Companion(rdev))
            }
            LinkOp::CompanionRelease { rdev, receipt } => {
                let entry = held.get_mut(&rdev)?;
                let read = CompanionRead::superseding(receipt, entry.journal.take_read());
                entry.journal = HeldJournal::Released { read };
                peer.journal.then_some(Send::CompanionRelease(rdev))
            }
        });
        if send.is_none() && held.is_empty() && owed.is_empty() {
            if let Some(ack) = ack {
                let _ = ack.try_send(false);
            }
            continue;
        }
        let fresh = client.is_none();
        if fresh && !heard && (ack.is_some() || std::time::Instant::now() >= next_try) {
            client = reconnect(peer, &mut held, &metas, &mut owed);
            if client.is_some() {
                backoff = Duration::from_millis(50);
            } else {
                next_try = std::time::Instant::now() + backoff;
                backoff = (backoff * 2).min(Duration::from_secs(5));
            }
        }
        let (Some(c), Some(send)) = (client.as_ref(), send) else {
            owe(&mut owed, must_reach);
            if let Some(ack) = ack {
                let _ = ack.try_send(false);
            }
            continue;
        };
        let mut sent = match &send {
            Send::Register(_) | Send::Companion(_) | Send::CompanionRelease(_) if fresh => Ok(()),
            Send::Frame(Frame::Meta { .. }) if fresh => Ok(()),
            Send::Register(rdev) => match held.get(rdev) {
                Some(h) => c.register(&h.master, h.header, h.tag.clone()),
                None => Ok(()),
            },
            Send::Companion(_) | Send::CompanionRelease(_) if !c.keeper_journals() => Ok(()),
            Send::Companion(rdev) => match held.get_mut(rdev) {
                Some(HeldMaster {
                    journal: HeldJournal::Region { region, len, read },
                    ..
                }) => send_companion(c, *rdev, region, *len, read),
                _ => Ok(()),
            },
            Send::CompanionRelease(rdev) => match held.get_mut(rdev) {
                Some(HeldMaster {
                    journal: HeldJournal::Released { read },
                    ..
                }) => send_companion_release(c, *rdev, read),
                _ => Ok(()),
            },
            Send::Frame(frame) => c.send(frame, None),
        };
        if sent.is_ok()
            && let Some((rdev, owe)) = must_reach.take()
        {
            if c.keeper_syncs() {
                match c.sync() {
                    Ok(n) => keep(
                        &mut owed,
                        rdev,
                        Owe {
                            sync: Some((n, std::time::Instant::now())),
                            ..owe
                        },
                    ),
                    Err(e) => {
                        sent = Err(e);
                        must_reach = Some((rdev, owe));
                    }
                }
            } else {
                owe.written_unsynced(std::time::Instant::now());
            }
        }
        if let Some(ack) = ack {
            let _ = ack.try_send(sent.is_ok());
        }
        if sent.is_err() {
            client = None;
            unconfirm(&mut owed);
            unconfirm_companions(&mut held);
            owe(&mut owed, must_reach);
        }
        if bye {
            return;
        }
    }
}

/// Owe `must_reach`'s RELEASE to the next connection.
fn owe(owed: &mut Owed, must_reach: Option<(u64, Owe)>) {
    if let Some((rdev, owe)) = must_reach {
        keep(owed, rdev, Owe { sync: None, ..owe });
    }
}

/// Owe `owe`'s RELEASE of `rdev`, merged with one already owed there. Past [`LINK_QUEUE`] owed
/// a tab's RELEASE is lost; a retirement never is (the boot cohort it belongs to is fixed).
fn keep(owed: &mut Owed, rdev: u64, owe: Owe) {
    let room = owed.len() < LINK_QUEUE || !owe.retires.is_empty();
    match owed.entry(rdev) {
        std::collections::btree_map::Entry::Occupied(mut slot) => {
            let was = slot.get_mut();
            was.sync = owe.sync;
            was.retires.extend(owe.retires);
        }
        std::collections::btree_map::Entry::Vacant(slot) => {
            if room {
                slot.insert(owe);
            }
        }
    }
}

/// Every owed RELEASE whose SYNC the keeper answered (`synced`) is done, and a retirement among
/// them confirmed.
fn settle_owed(owed: &mut Owed, synced: u64) {
    let now = std::time::Instant::now();
    owed.retain(|_, owe| match owe.sync {
        Some((n, _)) if n <= synced => {
            owe.confirm(now);
            false
        }
        _ => true,
    });
}

/// A REFUSED of `rdev` read with `synced_before` the highest SYNC answered: when it came before
/// the answer to the SYNC behind an owed retirement's RELEASE, it is that RELEASE's (or an
/// earlier frame's of that rdev, which is taken as its: no retirement is confirmed on a doubt).
/// One naming no record confirms the retirement, and the RELEASE is owed no more, since the
/// record went and the journal with it. Any other leaves the journal offerable, so the
/// retirement's observers are dropped unconfirmed, and the RELEASE stays owed as a plain one,
/// until its own SYNC is answered (the refusal may be an earlier frame's, the RELEASE still
/// unread). A REFUSED that is no owed retirement's leaves `owed` as it was.
fn refuse_retirement(owed: &mut Owed, rdev: u64, code: RefuseCode, synced_before: u64) {
    let Some(owe) = owed
        .get_mut(&rdev)
        .filter(|owe| !owe.retires.is_empty() && owe.sync.is_some_and(|(n, _)| synced_before < n))
    else {
        return;
    };
    if code == RefuseCode::NoSuchRecord {
        owe.confirm(std::time::Instant::now());
        owed.remove(&rdev);
    } else {
        owe.retires.clear();
    }
}

fn unconfirm(owed: &mut Owed) {
    for owe in owed.values_mut() {
        owe.sync = None;
    }
}

const fn unconfirm_companion(read: &mut Option<CompanionRead>) {
    if let Some(read) = read {
        read.fence = None;
    }
}

fn unconfirm_companions(held: &mut Held) {
    for h in held.values_mut() {
        if let HeldJournal::Region { read, .. } | HeldJournal::Released { read } = &mut h.journal {
            unconfirm_companion(read);
        }
    }
}

/// Write `frame` (a COMPANION or COMPANION_RELEASE), fenced by a SYNC on each side when `read`
/// waits on the keeper's read of it and the keeper answers SYNC.
fn send_fenced(
    c: &KeeperClient,
    read: &mut Option<CompanionRead>,
    frame: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    // An older keeper gives no read evidence. Leave the receipt pending rather than
    // treating a write, or absence of the capability, as permission to drop its region.
    if let Some(read) = read.as_mut()
        && c.keeper_syncs()
    {
        let after = c.sync()?;
        frame()?;
        let until = c.sync()?;
        read.fence = Some(CompanionFence {
            after,
            until,
            sent: std::time::Instant::now(),
            refused: false,
        });
        return Ok(());
    }
    frame()
}

fn send_companion(
    c: &KeeperClient,
    rdev: u64,
    region: &OwnedFd,
    len: u64,
    read: &mut Option<CompanionRead>,
) -> io::Result<()> {
    if !c.keeper_journals() {
        return Ok(());
    }
    send_fenced(c, read, || c.companion(rdev, region, len))
}

fn send_companion_release(
    c: &KeeperClient,
    rdev: u64,
    read: &mut Option<CompanionRead>,
) -> io::Result<()> {
    if !c.keeper_journals() {
        return Ok(());
    }
    send_fenced(c, read, || c.companion_release(rdev))
}

fn reconnect(
    peer: &LinkPeer,
    held: &mut Held,
    metas: &std::collections::BTreeMap<u64, Vec<u8>>,
    owed: &mut Owed,
) -> Option<KeeperClient> {
    let out = reconnect_inner(peer, held, metas, owed);
    if out.is_none() {
        unconfirm_companions(held);
    }
    out
}

fn reconnect_inner(
    peer: &LinkPeer,
    held: &mut Held,
    metas: &std::collections::BTreeMap<u64, Vec<u8>>,
    owed: &mut Owed,
) -> Option<KeeperClient> {
    unconfirm_companions(held);
    let c = KeeperClient::connect(&peer.path, &peer.identity, Duration::from_secs(2)).ok()?;
    c.hello_link(&peer.build, peer.marker.clone(), peer.journal)
        .ok()?;
    let journals = peer.journal && c.keeper_journals();
    for (rdev, h) in held.iter_mut() {
        c.register(&h.master, h.header, h.tag.clone()).ok()?;
        if journals {
            match &mut h.journal {
                HeldJournal::Region { region, len, read } => {
                    send_companion(&c, *rdev, region, *len, read).ok()?;
                }
                HeldJournal::Released { read } => {
                    send_companion_release(&c, *rdev, read).ok()?;
                }
                HeldJournal::Unsaid => {}
            }
        }
    }
    for (rdev, meta) in metas {
        c.send(
            &Frame::Meta {
                rdev: *rdev,
                meta: meta.clone(),
            },
            None,
        )
        .ok()?;
    }
    for rdev in owed.keys() {
        c.send(&Frame::Release { rdev: *rdev }, None).ok()?;
    }
    let now = std::time::Instant::now();
    if owed.is_empty() || !c.keeper_syncs() {
        for owe in owed.values() {
            owe.written_unsynced(now);
        }
        owed.clear();
    } else {
        let n = c.sync().ok()?;
        for owe in owed.values_mut() {
            owe.sync = Some((n, now));
        }
    }
    Some(c)
}

#[cfg(all(test, target_vendor = "apple"))]
mod tests {
    use super::*;
    use crate::server::{Relauncher, ServeConfig, Server};
    use crate::wire::Birth;
    use std::os::unix::fs::MetadataExt as _;
    use std::time::Instant;

    const TRUSTD_SILENT: &str =
        "the process trustd gate was silent — the machine's, not the code's";

    fn trustd_silent(why: &str) -> bool {
        why.starts_with("macOS's code-signing service (trustd) is not answering")
    }

    fn serve(sock: &Path) -> Server {
        Server::bind(ServeConfig {
            socket: sock.to_path_buf(),
            identity: Identity::same_uid(),
            relaunch: Relauncher::Unavailable,
            version: "test".into(),
            test_drop_custody: false,
        })
        .expect("bind")
    }

    fn step_until(server: &mut Server, what: &str, done: impl Fn(&Server) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !done(server) {
            assert!(Instant::now() < deadline, "never: {what}");
            server.step(20).expect("step");
        }
    }

    fn register_op(path: &str, local_id: u64) -> (u64, LinkOp) {
        let master = std::fs::File::open(path).expect("open");
        let rdev = master.metadata().expect("stat").rdev();
        let op = LinkOp::Register {
            master: OwnedFd::from(master),
            header: MasterHeader {
                rdev,
                shell_pid: std::process::id(),
                shell_birth: Birth::default(),
                local_id,
            },
            tag: Vec::new(),
        };
        (rdev, op)
    }

    #[test]
    fn a_listener_of_other_code_is_refused_as_not_this_build() {
        use std::os::unix::fs::PermissionsExt as _;
        aterm_uds::deadline::require_trust_service();
        let scratch = crate::test_scratch_dir("other");
        let dir = scratch.path();
        let own = Identity::for_self(crate::identity::IdentityPolicy::Designated).expect("own");

        let sock = dir.join("nc.sock");
        let mut nc = aterm_uds::spawnfd::command("/usr/bin/nc")
            .arg("-dlU")
            .arg(&sock)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn nc");
        let started = Instant::now();
        let refused = loop {
            match KeeperClient::connect(&sock, &own, Duration::from_secs(60)) {
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    assert!(started.elapsed().as_secs() < 60, "nc never listened: {e}");
                    std::thread::sleep(Duration::from_millis(5));
                }
                other => break other,
            }
        };
        let _ = nc.kill();
        let _ = nc.wait();
        let e = refused.expect_err("nc is not this build's code");
        if trustd_silent(&e.to_string()) {
            aterm_spec::host::could_not_run(format_args!("{TRUSTD_SILENT}: {e}"));
        }
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}");
        assert!(refused_identity(&e), "{e}");

        let absent = KeeperClient::connect(&dir.join("none.sock"), &own, Duration::from_secs(2))
            .expect_err("nothing there");
        assert!(!refused_identity(&absent), "{absent}");

        let closed = dir.join("closed.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&closed).expect("bind");
        std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        let denied = KeeperClient::connect(&closed, &own, Duration::from_secs(2))
            .expect_err("a socket this process may not open");
        assert_eq!(denied.kind(), io::ErrorKind::PermissionDenied, "{denied}");
        assert!(!refused_identity(&denied), "{denied}");
    }

    #[test]
    fn a_keeper_at_the_uid_floor_is_refused_by_a_window_that_checks_code() {
        aterm_uds::deadline::require_trust_service();
        let scratch = crate::test_scratch_dir("floor");
        let dir = scratch.path();
        let own = || Identity::for_self(IdentityPolicy::Designated).expect("own");
        for (name, keeper, accepted) in [
            ("uid.sock", Identity::same_uid(), false),
            ("designated.sock", own(), true),
        ] {
            let sock = dir.join(name);
            let mut server = Server::bind(ServeConfig {
                socket: sock.clone(),
                identity: keeper,
                relaunch: Relauncher::Unavailable,
                version: "test".into(),
                test_drop_custody: false,
            })
            .expect("bind");
            let window = std::thread::spawn(move || {
                let id = own();
                let dial = || KeeperClient::connect(&sock, &id, Duration::from_secs(60));
                let link = dial()?.hello_link("t", None, false);
                let boot = dial()?.hello_app("t").map(|_| ());
                let stat = dial()?.status("t").map(|_| ());
                Ok::<_, io::Error>((link, boot, stat))
            });
            while !window.is_finished() {
                server.step(20).expect("step");
            }
            let keeper_gate_silent = server
                .test_unchecked_ever()
                .iter()
                .any(|why| trustd_silent(why));
            let joined = window.join().expect("the window thread");
            let silent = |e: &io::Error| trustd_silent(&e.to_string()) || keeper_gate_silent;
            if let Err(e) = &joined
                && silent(e)
            {
                aterm_spec::host::could_not_run(format_args!("{TRUSTD_SILENT}: {name}: {e}"));
            }
            let (link, boot, stat) = joined.expect("the keeper's code is this build's");
            for (what, got) in [("link", link), ("boot", boot), ("status", stat)] {
                if let Err(e) = &got
                    && silent(e)
                {
                    aterm_spec::host::could_not_run(format_args!(
                        "{TRUSTD_SILENT}: {what} HELLO to {name}: {e}"
                    ));
                }
                if accepted {
                    got.unwrap_or_else(|e| panic!("{what} HELLO to {name}: {e}"));
                } else {
                    let e = got.expect_err(what);
                    assert!(refused_identity(&e), "{what} HELLO to {name}: {e}");
                    assert!(e.to_string().contains("--identity uid"), "{e}");
                }
            }
        }
    }

    #[test]
    fn only_a_refused_check_is_not_this_build() {
        let refused = check_failed(CheckError::Refused("OSStatus -67050".into()));
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        assert!(refused_identity(&refused), "{refused}");
        let unchecked = check_failed(CheckError::Unchecked(
            "the kernel did not give the peer's audit token".into(),
        ));
        assert!(!refused_identity(&unchecked), "{unchecked}");
        assert_eq!(
            unchecked.to_string(),
            "the kernel did not give the peer's audit token"
        );
        let unanswered = check_failed(CheckError::Unanswered(
            "macOS's code-signing service (trustd) is not answering".into(),
        ));
        assert!(!refused_identity(&unanswered), "{unanswered}");
        assert_eq!(unanswered.kind(), io::ErrorKind::Other, "{unanswered}");
    }

    #[test]
    fn a_restarted_keeper_is_told_again_without_a_new_op() {
        let scratch = crate::test_scratch_dir("restart");
        let sock = scratch.path().join("k.sock");
        let mut first = serve(&sock);
        let master = std::fs::File::open("/dev/null").expect("open");
        let rdev = master.metadata().expect("stat").rdev();
        let link = KeeperLink::start(sock.clone(), Identity::same_uid(), "t".into()).expect("link");
        assert!(link.enqueue(LinkOp::Register {
            master: OwnedFd::from(master),
            header: MasterHeader {
                rdev,
                shell_pid: std::process::id(),
                shell_birth: Birth::default(),
                local_id: 1,
            },
            tag: Vec::new(),
        }));
        step_until(&mut first, "the first keeper holds it", |s| s.holds(rdev));
        drop(first);
        let mut second = serve(&sock);
        step_until(&mut second, "the restarted keeper holds it", |s| {
            s.holds(rdev)
        });
        link.shutdown();
        drop(second);
    }

    #[test]
    fn a_restarted_keeper_is_told_each_region_again() {
        let scratch = crate::test_scratch_dir("journal");
        let sock = scratch.path().join("k.sock");
        let mut first = serve(&sock);
        let (journaled, journaled_op) = register_op("/dev/null", 1);
        let (unsaid, unsaid_op) = register_op("/dev/zero", 2);
        let link = KeeperLink::start_marked(
            sock.clone(),
            Identity::same_uid(),
            "t".into(),
            None,
            None,
            true,
        )
        .expect("link");
        assert!(link.enqueue(journaled_op));
        assert!(link.enqueue(unsaid_op));
        let len = aterm_uds::shm::PAGE;
        #[expect(
            clippy::disallowed_methods,
            reason = "a test playing the window makes the region the keeper is sent"
        )]
        // SAFETY: this fixture's link and keeper only transfer/hold/query custody, never reopen, map, write or resize it; `_region` remains inert until it is dropped.
        let (_region, custody) = unsafe { aterm_uds::shm::Region::create(len) }.expect("a region");
        assert!(link.enqueue(LinkOp::Companion {
            rdev: journaled,
            region: custody,
            len: len as u64,
            receipt: None,
        }));
        let view = |s: &Server, rdev| s.core().companion_view(rdev);
        let region_held = |s: &Server| {
            s.holds(journaled)
                && view(s, journaled)
                    .is_some_and(|v| v.offered.is_some() && v.reader_settled == Some(true))
        };
        step_until(&mut first, "the first keeper holds the region", region_held);
        drop(first);
        let mut second = serve(&sock);
        step_until(&mut second, "the restarted keeper holds it again", |s| {
            region_held(s) && s.holds(unsaid)
        });
        assert_eq!(
            view(&second, unsaid).map(|v| (v.offered, v.reader_settled)),
            Some((None, Some(false))),
            "a region not yet said is not told as none"
        );
        assert!(link.enqueue(LinkOp::CompanionRelease {
            rdev: journaled,
            receipt: None
        }));
        let released = |s: &Server| {
            s.holds(journaled)
                && view(s, journaled)
                    .is_some_and(|v| v.offered.is_none() && v.reader_settled == Some(true))
        };
        step_until(&mut second, "the release reaches it", released);
        drop(second);
        let mut third = serve(&sock);
        step_until(
            &mut third,
            "a third keeper is told it is released",
            released,
        );
        link.shutdown();
        drop(third);
    }

    #[test]
    fn a_replacement_companion_is_confirmed_only_once_the_keeper_has_read_it() {
        let scratch = crate::test_scratch_dir("receipt");
        let sock = scratch.path().join("k.sock");
        let mut server = serve(&sock);
        let (rdev, register) = register_op("/dev/null", 1);
        let link = KeeperLink::start_marked(
            sock.clone(),
            Identity::same_uid(),
            "t".into(),
            None,
            None,
            true,
        )
        .expect("link");
        assert!(link.enqueue(register));
        let len = aterm_uds::shm::PAGE;
        let region = || {
            #[expect(
                clippy::disallowed_methods,
                reason = "a test playing the window makes the regions the keeper is sent"
            )]
            // SAFETY: this fixture's link and keeper only transfer/hold/query custody, never reopen, map, write or resize it; the region remains inert until it is dropped.
            let made = unsafe { aterm_uds::shm::Region::create(len) }.expect("a region");
            made
        };
        let (_first, custody) = region();
        assert!(link.enqueue(LinkOp::Companion {
            rdev,
            region: custody,
            len: len as u64,
            receipt: None,
        }));
        let offered = |s: &Server| s.core().companion_view(rdev).and_then(|v| v.offered);
        step_until(&mut server, "the keeper holds the first region", |s| {
            s.holds(rdev) && offered(s).is_some()
        });
        let first = offered(&server);

        let (_fresh, custody) = region();
        let receipt = std::sync::Arc::new(CompanionReceipt::default());
        assert!(link.enqueue(LinkOp::Companion {
            rdev,
            region: custody,
            len: len as u64,
            receipt: Some(std::sync::Arc::clone(&receipt)),
        }));
        assert!(
            link.send_and_wait(
                LinkOp::Meta {
                    rdev,
                    meta: b"m".to_vec(),
                },
                Duration::from_secs(60),
            ),
            "control: the COMPANION, its SYNC and the META behind them are written"
        );
        assert!(
            !receipt.confirmed(),
            "a written COMPANION is not a read one"
        );
        assert_eq!(
            offered(&server),
            first,
            "premise: the keeper has read nothing since"
        );
        step_until(
            &mut server,
            "the keeper's read confirms the receipt",
            |_| receipt.confirmed(),
        );
        assert!(
            offered(&server).is_some_and(|id| Some(id) != first),
            "the keeper offers the fresh region"
        );
        link.shutdown();
        drop(server);
    }

    /// An idle wait no hang detector outlasts: a link idling this long wakes only for an op
    /// queued or a frame from its keeper.
    const IDLE_FOR_GOOD: Duration = Duration::from_secs(3_600);

    #[test]
    fn an_interrupted_doorbell_write_is_retried_before_the_worker_is_left_waiting() {
        use std::io::Read as _;

        let (bell, door) = doorbell().expect("doorbell");
        let mut writes = 0;
        bell.ring_with(|| {
            writes += 1;
            if writes == 1 {
                return Err(io::ErrorKind::Interrupted.into());
            }
            aterm_uds::fdpass::send_vectored(&bell.near, &[io::IoSlice::new(&[1])])
        });
        assert_eq!(writes, 2);
        let mut byte = [0];
        assert_eq!((&door.far).read(&mut byte).expect("the wake byte"), 1);
        assert_eq!(byte, [1]);
    }

    #[test]
    fn a_full_doorbell_already_wakes_the_worker_and_coalesces_later_rings() {
        let (bell, mut door) = doorbell().expect("doorbell");
        let bytes = [1];
        loop {
            match aterm_uds::fdpass::send_vectored(&bell.near, &[io::IoSlice::new(&bytes)]) {
                Ok(n) => assert!(n > 0),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => panic!("fill the door: {e}"),
            }
        }
        let mut writes = 0;
        bell.ring_with(|| {
            writes += 1;
            let e = aterm_uds::fdpass::send_vectored(&bell.near, &[io::IoSlice::new(&[1])])
                .expect_err("the door is full");
            assert_eq!(e.kind(), io::ErrorKind::WouldBlock);
            Err(e)
        });
        assert_eq!(writes, 1);
        bell.ring_with(|| panic!("an unanswered ring is coalesced"));
        door.answer();
        // Once answered, the next queued op must be able to ring again.
        let mut writes = 0;
        bell.ring_with(|| {
            writes += 1;
            aterm_uds::fdpass::send_vectored(&bell.near, &[io::IoSlice::new(&[1])])
        });
        assert_eq!(writes, 1);
        use std::io::Read as _;
        let mut byte = [0];
        assert_eq!((&door.far).read(&mut byte).expect("a fresh wake byte"), 1);
        assert_eq!(byte, [1]);
    }

    #[test]
    fn a_doorbell_write_that_writes_nothing_does_not_suppress_the_next_ring() {
        for failure in [Ok(0), Err(io::ErrorKind::BrokenPipe.into())] {
            let (bell, door) = doorbell().expect("doorbell");
            let mut failure = Some(failure);
            bell.ring_with(|| failure.take().expect("one refused write"));
            bell.ring();
            use std::io::Read as _;
            let mut byte = [0];
            assert_eq!((&door.far).read(&mut byte).expect("a later wake byte"), 1);
            assert_eq!(byte, [1]);
        }
    }

    /// A recovered window's fence lifts on the keeper's SYNC answer (K1): the worker reads it as
    /// it arrives, not on its next op or its next idle wake, neither of which comes here.
    #[test]
    fn a_receipt_is_confirmed_as_the_keeper_answers_with_no_op_behind_it() {
        let scratch = crate::test_scratch_dir("answer");
        let sock = scratch.path().join("k.sock");
        let mut server = serve(&sock);
        let (rdev, register) = register_op("/dev/null", 1);
        let link = KeeperLink::start_idling(
            sock.clone(),
            Identity::same_uid(),
            "t".into(),
            None,
            None,
            true,
            IDLE_FOR_GOOD,
        )
        .expect("link");
        assert!(link.enqueue(register));
        let len = aterm_uds::shm::PAGE;
        #[expect(
            clippy::disallowed_methods,
            reason = "a test playing the window makes the region the keeper is sent"
        )]
        // SAFETY: this fixture's link and keeper only transfer/hold/query custody, never reopen, map, write or resize it; the region remains inert until it is dropped.
        let (_region, custody) = unsafe { aterm_uds::shm::Region::create(len) }.expect("a region");
        let receipt = std::sync::Arc::new(CompanionReceipt::default());
        assert!(link.enqueue(LinkOp::Companion {
            rdev,
            region: custody,
            len: len as u64,
            receipt: Some(std::sync::Arc::clone(&receipt)),
        }));
        step_until(
            &mut server,
            "the keeper's answer to the SYNC behind the COMPANION confirms the receipt",
            |_| receipt.confirmed(),
        );
        assert!(
            server
                .core()
                .companion_view(rdev)
                .is_some_and(|v| v.offered.is_some()),
            "the keeper holds the region it confirmed"
        );
        let (ended_tx, ended) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            link.shutdown();
            let _ = ended_tx.send(());
        });
        assert!(
            ended.recv_timeout(aterm_spec::wait::MUST_HAPPEN).is_ok(),
            "a link shut down ends without waiting out its idle"
        );
        drop(server);
    }

    /// A keeper that welcomes a link and then closes it is tried again on the link's next op or
    /// its idle wait, never on the wake its close caused: a worker that dialled there would dial
    /// a keeper dropping every link it welcomes as fast as it could, with no pause between.
    #[test]
    fn a_keeper_that_closes_the_link_is_not_dialled_again_on_its_close() {
        use crate::wire::Kind;
        use std::sync::atomic::{AtomicBool, Ordering};
        /// Read `stream`'s frames, welcoming its HELLO, up to and including one of kind `last`.
        fn kinds_until(stream: &std::os::unix::net::UnixStream, last: Kind) -> Vec<Kind> {
            stream
                .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
                .expect("its hang detector");
            let mut kinds = Vec::new();
            loop {
                let frame = read_frame(stream).expect("read").expect("a frame").frame;
                kinds.push(frame.kind());
                if matches!(frame, Frame::Hello { .. }) {
                    let welcome = Frame::Welcome {
                        proto: PROTO,
                        offers: 0,
                        version: "closing".into(),
                        identity: Some(IdentityPolicy::SameUid),
                        caps: Some(0),
                    };
                    write_frame(stream, &welcome, None).expect("welcome");
                }
                if frame.kind() == last {
                    return kinds;
                }
            }
        }
        let scratch = crate::test_scratch_dir("closed");
        let sock = scratch.path().join("k.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
        let asked = std::sync::Arc::new(AtomicBool::new(false));
        let (closed_tx, closed) = mpsc::sync_channel(1);
        let keeper = {
            let asked = std::sync::Arc::clone(&asked);
            std::thread::spawn(move || {
                let (first, _) = aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Forever,
                    aterm_uds::AcceptedMode::Blocking,
                )
                .expect("the first link");
                let first_kinds = kinds_until(&first, Kind::Register);
                drop(first);
                closed_tx.send(()).expect("the test waits for the close");
                let (second, _) = aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Forever,
                    aterm_uds::AcceptedMode::Blocking,
                )
                .expect("the second link");
                let unasked = !asked.load(Ordering::SeqCst);
                (first_kinds, unasked, kinds_until(&second, Kind::Pending))
            })
        };
        let (_, register) = register_op("/dev/null", 1);
        let link = KeeperLink::start_idling(
            sock,
            Identity::same_uid(),
            "t".into(),
            None,
            None,
            false,
            IDLE_FOR_GOOD,
        )
        .expect("link");
        assert!(link.enqueue(register));
        closed
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the keeper read the REGISTER and closed the link");
        // Neither an op nor the link's idle wait comes here, so a dial in this pause is one the
        // close's own wake made. Its length is only how sure the test is to see one.
        std::thread::sleep(LINK_POLL);
        asked.store(true, Ordering::SeqCst);
        assert!(
            link.send_and_wait(LinkOp::Pending { pid: 1 }, aterm_spec::wait::MUST_HAPPEN),
            "a waited op dials the keeper at once"
        );
        let (first, unasked, second) = keeper.join().expect("the closing keeper");
        assert_eq!(first, [Kind::Hello, Kind::Register]);
        assert!(
            !unasked,
            "the link dialled the keeper again on its close, with no op queued and its idle wait \
             not over"
        );
        assert_eq!(second, [Kind::Hello, Kind::Register, Kind::Pending]);
        link.shutdown();
    }

    #[test]
    fn a_release_the_link_could_not_write_is_told_to_the_next_connection() {
        let scratch = crate::test_scratch_dir("rel");
        let dir = scratch.path();
        let sock = dir.join("k.sock");
        let aside = dir.join("k.aside");
        let mut server = serve(&sock);
        let (closed, closed_op) = register_op("/dev/null", 1);
        let (kept, kept_op) = register_op("/dev/zero", 2);
        assert_ne!(closed, kept);
        let link = KeeperLink::start(sock.clone(), Identity::same_uid(), "t".into()).expect("link");
        assert!(link.enqueue(closed_op));
        assert!(link.enqueue(kept_op));
        step_until(&mut server, "the keeper holds both", |s| {
            s.holds(closed) && s.holds(kept)
        });
        std::fs::rename(&sock, &aside).expect("move the keeper's socket aside");
        server.test_drop_connections();
        let written = link.send_and_wait(LinkOp::Release { rdev: closed }, Duration::from_secs(60));
        assert!(!written, "no keeper was reachable");
        std::fs::rename(&aside, &sock).expect("restore the keeper's socket");
        step_until(&mut server, "the RELEASE reaches the keeper", |s| {
            !s.holds(closed)
        });
        assert!(server.holds(kept), "the open tab stays in custody");
        link.shutdown();
        drop(server);
    }

    #[test]
    fn a_release_the_keeper_dropped_unread_is_told_to_the_next_connection() {
        let scratch = crate::test_scratch_dir("unread");
        let sock = scratch.path().join("k.sock");
        let mut server = serve(&sock);
        let (closed, closed_op) = register_op("/dev/null", 1);
        let (kept, kept_op) = register_op("/dev/zero", 2);
        let link = KeeperLink::start(sock.clone(), Identity::same_uid(), "t".into()).expect("link");
        assert!(link.enqueue(closed_op));
        assert!(link.enqueue(kept_op));
        step_until(&mut server, "the keeper holds both", |s| {
            s.holds(closed) && s.holds(kept)
        });
        let written = link.send_and_wait(LinkOp::Release { rdev: closed }, Duration::from_secs(60));
        assert!(written, "the RELEASE was written");
        server.test_drop_connections();
        assert!(server.holds(closed), "the keeper never read the RELEASE");
        step_until(&mut server, "the RELEASE reaches the keeper", |s| {
            !s.holds(closed)
        });
        assert!(server.holds(kept), "the open tab stays in custody");
        link.shutdown();
        drop(server);
    }

    #[test]
    fn a_welcome_later_than_a_frame_still_answers_the_hello() {
        #[derive(Clone, Copy, Debug)]
        enum Hello {
            App,
            Link,
            Status,
        }
        let frame = Duration::from_millis(100);
        let late = Duration::from_millis(400);
        let dials: Vec<(usize, Hello)> = [Hello::App, Hello::Link, Hello::Status]
            .into_iter()
            .enumerate()
            .collect();
        aterm_spec::check::each(&dials, |&(n, hello)| {
            let scratch = crate::test_scratch_dir(&format!("late-{n}"));
            let sock = scratch.path().join("k.sock");
            let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
            let keeper = std::thread::spawn(move || {
                let (stream, _) = aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Forever,
                    aterm_uds::AcceptedMode::Blocking,
                )
                .expect("accept");
                stream
                    .set_read_timeout(Some(Duration::from_secs(60)))
                    .expect("timeout");
                let hello = read_frame(&stream).expect("read").expect("a frame").frame;
                assert!(matches!(hello, Frame::Hello { .. }), "{hello:?}");
                std::thread::sleep(late);
                let welcome = Frame::Welcome {
                    proto: PROTO,
                    offers: 0,
                    version: "late".into(),
                    identity: Some(IdentityPolicy::SameUid),
                    caps: Some(0),
                };
                write_frame(&stream, &welcome, None).expect("welcome");
                if matches!(
                    hello,
                    Frame::Hello {
                        class: PeerClass::Status,
                        ..
                    }
                ) {
                    let ask = read_frame(&stream).expect("read").expect("a frame").frame;
                    assert!(matches!(ask, Frame::Status { .. }), "{ask:?}");
                    std::thread::sleep(late);
                    let status = Frame::Status {
                        text: "late".into(),
                    };
                    write_frame(&stream, &status, None).expect("status");
                }
                stream
            });
            let client = KeeperClient::connect(&sock, &Identity::same_uid(), frame).expect("dial");
            match hello {
                Hello::App => {
                    let offers = client
                        .hello_app_marked("t", None, true)
                        .expect("the late WELCOME is heard");
                    assert!(offers.is_empty());
                }
                Hello::Link => client
                    .hello_link("t", None, true)
                    .expect("the late WELCOME is heard"),
                Hello::Status => assert_eq!(
                    client
                        .status("t")
                        .expect("the late WELCOME and STATUS are heard"),
                    "late"
                ),
            }
            assert_eq!(
                client.stream.read_timeout().expect("its wait"),
                Some(frame),
                "{hello:?}"
            );
            drop(keeper.join().expect("the late keeper"));
        });
    }

    #[test]
    fn a_keeper_that_answers_no_sync_is_sent_none() {
        use crate::wire::Kind;
        let scratch = crate::test_scratch_dir("nosync");
        let sock = scratch.path().join("k.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
        let older = std::thread::spawn(move || {
            let (stream, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .expect("accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(60)))
                .expect("timeout");
            let mut kinds = Vec::new();
            loop {
                let frame = read_frame(&stream).expect("read").expect("a frame").frame;
                kinds.push(frame.kind());
                match frame {
                    Frame::Hello { .. } => {
                        let welcome = Frame::Welcome {
                            proto: PROTO,
                            offers: 0,
                            version: "older".into(),
                            identity: Some(IdentityPolicy::SameUid),
                            caps: Some(CAP_JOURNAL),
                        };
                        write_frame(&stream, &welcome, None).expect("welcome");
                    }
                    Frame::Pending { .. } => return kinds,
                    _ => {}
                }
            }
        });
        let (rdev, op) = register_op("/dev/null", 1);
        let link = KeeperLink::start(sock, Identity::same_uid(), "t".into()).expect("link");
        assert!(link.enqueue(op));
        let within = Duration::from_secs(60);
        assert!(link.send_and_wait(LinkOp::Release { rdev }, within));
        let retired = std::sync::Arc::new(ReadObserver::default());
        assert!(link.send_and_wait(
            LinkOp::Retire {
                rdev: rdev + 1,
                observer: std::sync::Arc::clone(&retired),
            },
            within
        ));
        assert!(
            retired.confirmed(),
            "to a keeper without CAP_SYNC a written RELEASE is done"
        );
        assert!(link.send_and_wait(LinkOp::Pending { pid: 1 }, within));
        let kinds = older.join().expect("the older keeper");
        assert_eq!(
            kinds,
            [
                Kind::Hello,
                Kind::Register,
                Kind::Release,
                Kind::Release,
                Kind::Pending
            ]
        );
        link.shutdown();
    }

    /// A link already speaking to `server`'s keeper (its REGISTER of the device at `own` held
    /// there), and that master's rdev.
    fn linked(server: &mut Server, sock: &Path, idle: Duration, own: &str) -> (KeeperLink, u64) {
        let (own, register) = register_op(own, 9);
        let link = KeeperLink::start_idling(
            sock.to_path_buf(),
            Identity::same_uid(),
            "t".into(),
            None,
            None,
            true,
            idle,
        )
        .expect("link");
        assert!(link.enqueue(register));
        step_until(server, "the link's keeper holds its master", |s| {
            s.holds(own)
        });
        (link, own)
    }

    /// A window's boot connection to `server`'s keeper: its HELLO answered, no offers.
    fn booted(server: &mut Server, sock: &Path) -> KeeperClient {
        let sock = sock.to_path_buf();
        let hello = std::thread::spawn(move || {
            let c = KeeperClient::connect(&sock, &Identity::same_uid(), Duration::from_secs(60))
                .expect("dial");
            let offers = c.hello_app_marked("t", None, true).expect("the WELCOME");
            assert!(offers.is_empty(), "premise: nothing to offer");
            c
        });
        step_until(server, "the boot HELLO is answered", |_| {
            hello.is_finished()
        });
        hello.join().expect("the boot HELLO")
    }

    /// The window retires a journal its keeper still holds ([`LinkOp::Retire`]): the observer
    /// hears of it once the keeper answers the SYNC behind the RELEASE, and not while the keeper
    /// holds the connection unread past their write. The negative control's target: a link that
    /// confirms on the RELEASE's write is caught here.
    #[test]
    fn a_retirement_is_confirmed_only_once_the_keeper_answers_the_sync_behind_its_release() {
        let scratch = crate::test_scratch_dir("retire");
        let sock = scratch.path().join("k.sock");
        let mut server = serve(&sock);
        let (older, _) = linked(&mut server, &sock, IDLE_FOR_GOOD, "/dev/zero");
        let (rdev, register) = register_op("/dev/null", 1);
        assert!(older.enqueue(register));
        step_until(&mut server, "the keeper holds the old master", |s| {
            s.holds(rdev)
        });
        let (link, _) = linked(&mut server, &sock, IDLE_FOR_GOOD, "/dev/random");
        let observer = std::sync::Arc::new(ReadObserver::default());
        assert!(
            link.send_and_wait(
                LinkOp::Retire {
                    rdev,
                    observer: std::sync::Arc::clone(&observer),
                },
                Duration::from_secs(60),
            ),
            "control: the RELEASE and its SYNC are written"
        );
        assert!(
            server.holds(rdev),
            "premise: the keeper has read nothing since"
        );
        assert!(
            !observer.confirmed(),
            "a written RELEASE is not a retirement the keeper confirmed"
        );
        step_until(&mut server, "the keeper's answer confirms it", |_| {
            observer.confirmed()
        });
        assert!(!server.holds(rdev), "the keeper closed its copy");
        let read = observer.confirmation().expect("its read");
        assert!(read.sync_sent <= read.answer_read);
        link.shutdown();
        older.shutdown();
    }

    /// A REFUSED that is no owed retirement's moves nothing owed: not a plain RELEASE (owed until
    /// its own SYNC is answered), and not a retirement whose SYNC this connection never wrote or
    /// had answered before the refusal was read.
    #[test]
    fn a_refusal_that_is_no_owed_retirement_s_leaves_what_is_owed() {
        let now = std::time::Instant::now();
        let observer = std::sync::Arc::new(ReadObserver::default());
        let mut owed = Owed::new();
        owed.insert(
            5,
            Owe {
                sync: Some((3, now)),
                retires: Vec::new(),
            },
        );
        owed.insert(
            6,
            Owe {
                sync: None,
                retires: vec![std::sync::Arc::clone(&observer)],
            },
        );
        owed.insert(
            7,
            Owe {
                sync: Some((2, now)),
                retires: vec![std::sync::Arc::clone(&observer)],
            },
        );
        refuse_retirement(&mut owed, 5, RefuseCode::NotClaimant, 0);
        refuse_retirement(&mut owed, 6, RefuseCode::NoSuchRecord, 0);
        refuse_retirement(&mut owed, 7, RefuseCode::NoSuchRecord, 2);
        assert_eq!(owed.keys().copied().collect::<Vec<_>>(), [5, 6, 7]);
        assert!(!observer.confirmed(), "no retirement was refused");
        refuse_retirement(&mut owed, 7, RefuseCode::NoSuchRecord, 1);
        assert_eq!(owed.keys().copied().collect::<Vec<_>>(), [5, 6]);
        assert!(
            observer.confirmed(),
            "control: the retirement's own refusal confirms it"
        );
    }

    /// A retirement's RELEASE refused for any reason but naming no record (or an earlier frame of
    /// its rdev refused, which is taken as its) drops its observers unconfirmed, since the keeper
    /// may still offer that journal, but stays owed as a plain RELEASE until its own SYNC is
    /// answered: the refused frame may be the master's COMPANION, its RELEASE still unread.
    #[test]
    fn a_refused_retirement_stays_owed_as_a_plain_release() {
        let now = std::time::Instant::now();
        let observer = std::sync::Arc::new(ReadObserver::default());
        let mut owed = Owed::new();
        owed.insert(
            8,
            Owe {
                sync: Some((4, now)),
                retires: vec![std::sync::Arc::clone(&observer)],
            },
        );
        refuse_retirement(&mut owed, 8, RefuseCode::NotClaimant, 3);
        assert!(
            owed.get(&8).is_some_and(|owe| owe.retires.is_empty()),
            "owed still, as a plain RELEASE"
        );
        refuse_retirement(&mut owed, 8, RefuseCode::NoSuchRecord, 3);
        settle_owed(&mut owed, 3);
        assert!(owed.contains_key(&8), "its own SYNC is not yet answered");
        settle_owed(&mut owed, 4);
        assert!(owed.is_empty(), "its SYNC's answer settles it");
        assert!(
            !observer.confirmed(),
            "a refused retirement is never confirmed, not by a later refusal nor by its answer"
        );
    }

    /// A RELEASE the keeper refuses because it holds the master for another claim leaves that
    /// journal offerable, so its retirement is never confirmed, though the SYNC behind it is
    /// answered; a RELEASE naming no record confirms its retirement (the journal went with it).
    #[test]
    fn a_retirement_the_keeper_refuses_for_another_claim_is_not_confirmed() {
        let scratch = crate::test_scratch_dir("refuse");
        let sock = scratch.path().join("k.sock");
        let mut server = serve(&sock);
        let (link, _) = linked(&mut server, &sock, IDLE_FOR_GOOD, "/dev/zero");
        let (newer, _) = linked(&mut server, &sock, IDLE_FOR_GOOD, "/dev/random");
        let (claimed, register) = register_op("/dev/null", 1);
        assert!(newer.enqueue(register));
        step_until(&mut server, "a newer link claims the master", |s| {
            s.holds(claimed)
        });
        let held_elsewhere = std::sync::Arc::new(ReadObserver::default());
        let gone = std::sync::Arc::new(ReadObserver::default());
        for (rdev, observer) in [(claimed, &held_elsewhere), (claimed ^ 0x5a5a, &gone)] {
            assert!(link.enqueue(LinkOp::Retire {
                rdev,
                observer: std::sync::Arc::clone(observer),
            }));
        }
        step_until(&mut server, "the RELEASE of no record confirms", |_| {
            gone.confirmed()
        });
        assert!(
            !held_elsewhere.confirmed(),
            "the keeper keeps the master for the newer claim: its journal is still offerable"
        );
        assert!(server.holds(claimed), "the newer claim keeps its master");
        link.shutdown();
        newer.shutdown();
    }

    /// A retirement whose keeper ended before answering is told to the next keeper, which, holding
    /// no record of the master, confirms it.
    #[test]
    fn a_retirement_unanswered_when_its_keeper_ended_is_told_to_the_next_one() {
        let scratch = crate::test_scratch_dir("reretire");
        let sock = scratch.path().join("k.sock");
        let mut first = serve(&sock);
        let (link, zero) = linked(&mut first, &sock, LINK_POLL, "/dev/zero");
        let observer = std::sync::Arc::new(ReadObserver::default());
        assert!(link.send_and_wait(
            LinkOp::Retire {
                rdev: zero ^ 0x5a5a,
                observer: std::sync::Arc::clone(&observer),
            },
            Duration::from_secs(60),
        ));
        drop(first);
        assert!(!observer.confirmed(), "the first keeper read nothing");
        let mut second = serve(&sock);
        step_until(&mut second, "the next keeper confirms it", |_| {
            observer.confirmed()
        });
        assert!(second.holds(zero), "and holds the link's master again");
        link.shutdown();
    }

    /// The boot connection's RELEASE of an offer the window refused ([`KeeperClient::retire`]) is
    /// seen through by the link started on that connection, once the keeper answers its SYNC.
    #[test]
    fn a_boot_connection_s_retirement_is_seen_through_by_its_link() {
        let scratch = crate::test_scratch_dir("bootret");
        let sock = scratch.path().join("k.sock");
        let mut server = serve(&sock);
        let (older, _) = linked(&mut server, &sock, IDLE_FOR_GOOD, "/dev/zero");
        let (rdev, register) = register_op("/dev/null", 1);
        assert!(older.enqueue(register));
        step_until(&mut server, "the keeper holds the old master", |s| {
            s.holds(rdev)
        });
        let boot = booted(&mut server, &sock);
        let observer = std::sync::Arc::new(ReadObserver::default());
        boot.retire(rdev, std::sync::Arc::clone(&observer))
            .expect("written");
        let link = KeeperLink::start_idling(
            sock.clone(),
            Identity::same_uid(),
            "t".into(),
            None,
            Some(boot),
            true,
            IDLE_FOR_GOOD,
        )
        .expect("link");
        assert!(!observer.confirmed(), "the keeper has not read the RELEASE");
        step_until(&mut server, "the link reads the keeper's answer", |_| {
            observer.confirmed()
        });
        assert!(!server.holds(rdev), "the keeper closed its copy");
        link.shutdown();
        older.shutdown();
    }

    /// An adopted session with no journal tells the keeper so (COMPANION_RELEASE), and the
    /// receipt it carries is confirmed only once the keeper has read it.
    #[test]
    fn a_companion_release_is_confirmed_only_once_the_keeper_has_read_it() {
        let scratch = crate::test_scratch_dir("relrcpt");
        let sock = scratch.path().join("k.sock");
        let mut server = serve(&sock);
        let (link, _) = linked(&mut server, &sock, IDLE_FOR_GOOD, "/dev/zero");
        let (rdev, register) = register_op("/dev/null", 1);
        assert!(link.enqueue(register));
        let observer = std::sync::Arc::new(ReadObserver::default());
        assert!(
            link.send_and_wait(
                LinkOp::CompanionRelease {
                    rdev,
                    receipt: Some(std::sync::Arc::new(
                        CompanionReceipt::reporting_confirmation(std::sync::Arc::clone(&observer))
                    )),
                },
                Duration::from_secs(60),
            ),
            "control: the REGISTER and the fenced COMPANION_RELEASE are written"
        );
        assert!(
            !server.holds(rdev),
            "premise: the keeper has read nothing since"
        );
        assert!(
            !observer.confirmed(),
            "a written COMPANION_RELEASE is not a read one"
        );
        step_until(&mut server, "the keeper's read confirms it", |_| {
            observer.confirmed()
        });
        assert_eq!(
            server
                .core()
                .companion_view(rdev)
                .map(|v| (v.offered, v.reader_settled)),
            Some((None, Some(true))),
            "the keeper holds no region an offer of it could carry"
        );
        link.shutdown();
    }

    /// A journal region a window would send, and its read-only custody descriptor.
    fn journal_region() -> (aterm_uds::shm::Region, OwnedFd) {
        #[expect(
            clippy::disallowed_methods,
            reason = "a test playing the window makes the region the keeper is sent"
        )]
        // SAFETY: this fixture's link and keeper only transfer/hold/query custody, never reopen, map, write or resize it; the returned region remains inert until it is dropped.
        let made = unsafe { aterm_uds::shm::Region::create(aterm_uds::shm::PAGE) };
        made.expect("a region")
    }

    /// A held master's journal frame that reports to a [`ReadObserver`], superseded before the
    /// link read the keeper's answer to the SYNC behind it, hands that observer on to the frame
    /// that superseded it. A tab's RELEASE: the keeper's read closes the record, and the old
    /// journal with it. A receipt-less COMPANION_RELEASE (the journal stopped): its read leaves
    /// the keeper no region an offer could carry. Every frame is written before the keeper reads
    /// one, as when the keeper is slow or the link is between connections. Before, the observer
    /// went with the superseded read, and a recovered window's fence stood for its run.
    #[test]
    fn a_journal_frame_superseded_before_its_answer_hands_its_observer_on() {
        let scratch = crate::test_scratch_dir("supersede");
        let sock = scratch.path().join("k.sock");
        let mut server = serve(&sock);
        let (link, _) = linked(&mut server, &sock, IDLE_FOR_GOOD, "/dev/zero");
        let within = Duration::from_secs(60);
        let reporting = |observer: &std::sync::Arc<ReadObserver>| {
            Some(std::sync::Arc::new(
                CompanionReceipt::reporting_confirmation(std::sync::Arc::clone(observer)),
            ))
        };
        for (case, journaled, stops) in [
            ("a session with no journal closed", false, false),
            ("a journaled session closed", true, false),
            ("a session's journal stopped", true, true),
        ] {
            let (rdev, register) = register_op("/dev/null", 1);
            let observer = std::sync::Arc::new(ReadObserver::default());
            let (_region, custody) = journal_region();
            let journal = if journaled {
                LinkOp::Companion {
                    rdev,
                    region: custody,
                    len: aterm_uds::shm::PAGE as u64,
                    receipt: reporting(&observer),
                }
            } else {
                LinkOp::CompanionRelease {
                    rdev,
                    receipt: reporting(&observer),
                }
            };
            let next = if stops {
                LinkOp::CompanionRelease {
                    rdev,
                    receipt: None,
                }
            } else {
                LinkOp::Release { rdev }
            };
            assert!(link.enqueue(register), "{case}");
            assert!(link.enqueue(journal), "{case}");
            assert!(
                link.send_and_wait(next, within),
                "{case}: control: every frame is written"
            );
            assert!(
                !server.holds(rdev),
                "{case}: premise: the keeper has read none of them"
            );
            assert!(!observer.confirmed(), "{case}: nothing unread is confirmed");
            step_until(&mut server, case, |_| observer.confirmed());
            if stops {
                assert_eq!(
                    server
                        .core()
                        .companion_view(rdev)
                        .map(|v| (v.offered, v.reader_settled)),
                    Some((None, Some(true))),
                    "{case}: the keeper holds no region an offer of it could carry"
                );
                assert!(link.send_and_wait(LinkOp::Release { rdev }, within));
            }
            step_until(&mut server, "the record is closed", |s| !s.holds(rdev));
        }
        link.shutdown();
    }

    /// A REGISTER at the rdev of a retirement still owed confirms it at once: the kernel gives a
    /// pty's device to another master only once every descriptor of the old one, the keeper's
    /// custody copy among them, is closed, so the old record and its journal are gone. The keeper
    /// has read nothing when the link takes the REGISTER, so nothing else could confirm it.
    #[test]
    fn a_register_at_a_retired_rdev_confirms_the_retirement() {
        let scratch = crate::test_scratch_dir("reuse");
        let sock = scratch.path().join("k.sock");
        let mut server = serve(&sock);
        let (link, _) = linked(&mut server, &sock, IDLE_FOR_GOOD, "/dev/zero");
        let (rdev, register) = register_op("/dev/null", 1);
        let observer = std::sync::Arc::new(ReadObserver::default());
        let within = Duration::from_secs(60);
        assert!(
            link.send_and_wait(
                LinkOp::Retire {
                    rdev,
                    observer: std::sync::Arc::clone(&observer),
                },
                within,
            ),
            "control: the RELEASE and its SYNC are written"
        );
        assert!(!observer.confirmed(), "premise: nothing has confirmed it");
        assert!(
            link.send_and_wait(register, within),
            "the REGISTER at its rdev is written"
        );
        assert!(
            !server.holds(rdev),
            "premise: the keeper has still read nothing"
        );
        let read = observer
            .confirmation()
            .expect("the REGISTER at its rdev confirms the retirement");
        assert!(read.sync_sent <= read.answer_read);
        step_until(&mut server, "the new master's REGISTER is read", |s| {
            s.holds(rdev)
        });
        link.shutdown();
    }

    #[test]
    fn pending_and_bye_are_acknowledged_once_written() {
        let scratch = crate::test_scratch_dir("ack");
        let sock = scratch.path().join("k.sock");
        let mut server = serve(&sock);
        let master = std::fs::File::open("/dev/null").expect("open");
        let rdev = master.metadata().expect("stat").rdev();
        let link = KeeperLink::start(sock.clone(), Identity::same_uid(), "t".into()).expect("link");
        assert!(link.enqueue(LinkOp::Register {
            master: OwnedFd::from(master),
            header: MasterHeader {
                rdev,
                shell_pid: std::process::id(),
                shell_birth: Birth::default(),
                local_id: 1,
            },
            tag: Vec::new(),
        }));
        let waiter = std::thread::spawn(move || {
            let pending = link.send_and_wait(LinkOp::Pending { pid: 1 }, Duration::from_secs(60));
            let bye = link.send_and_wait(LinkOp::Bye, Duration::from_secs(60));
            let after = link.send_and_wait(LinkOp::Release { rdev }, Duration::from_millis(200));
            (pending, bye, after)
        });
        step_until(&mut server, "the keeper read the BYE", |s| {
            s.core().peers().values().any(|p| p.bye)
        });
        let (pending, bye, after) = waiter.join().expect("waiter");
        assert!(pending && bye, "written: pending={pending} bye={bye}");
        assert!(!after, "no op is written after the BYE");
        assert_eq!(
            server
                .core()
                .pending_successors()
                .iter()
                .map(|p| p.pid)
                .collect::<Vec<_>>(),
            [1],
            "the PENDING arrived before the BYE"
        );
        assert!(server.holds(rdev), "the RELEASE after the BYE never came");
        drop(server);
    }

    /// A lasting wait that runs out leaves its op queued, to be written in its turn; an ordinary
    /// waited op whose wait ran out is never written.
    #[test]
    fn a_lasting_waited_op_outlives_its_wait() {
        let (link, far) = KeeperLink::stalled();
        assert!(
            !link.send_lasting_and_wait(LinkOp::Release { rdev: 7 }, Duration::ZERO),
            "no keeper wrote it"
        );
        assert!(!link.send_and_wait(LinkOp::Release { rdev: 8 }, Duration::from_millis(1)));
        let queued: Vec<u64> = far
            .drain()
            .into_iter()
            .filter_map(|op| match op {
                LinkOp::Release { rdev } => Some(rdev),
                _ => None,
            })
            .collect();
        assert_eq!(
            queued,
            [7],
            "the lasting op stays queued; the ordinary one went with its wait"
        );
    }

    #[test]
    fn a_waited_op_with_no_keeper_is_refused_promptly() {
        let scratch = crate::test_scratch_dir("none");
        let sock = scratch.path().join("absent.sock");
        let link = KeeperLink::start(sock, Identity::same_uid(), "t".into()).expect("link");
        let master = std::fs::File::open("/dev/null").expect("open");
        let rdev = master.metadata().expect("stat").rdev();
        assert!(link.enqueue(LinkOp::Register {
            master: OwnedFd::from(master),
            header: MasterHeader {
                rdev,
                shell_pid: std::process::id(),
                shell_birth: Birth::default(),
                local_id: 1,
            },
            tag: Vec::new(),
        }));
        let started = Instant::now();
        assert!(!link.send_and_wait(LinkOp::Pending { pid: 1 }, Duration::from_secs(300)));
        assert!(
            started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "refused without waiting out the bound: {:?}",
            started.elapsed()
        );
        link.shutdown();
    }

    #[test]
    fn a_refused_companion_does_not_poison_a_later_valid_companion_on_the_same_link() {
        let scratch = crate::test_scratch_dir("rc");
        let sock = scratch.path().join("k.sock");
        let mut server = serve(&sock);
        let (rdev, register) = register_op("/dev/null", 1);
        let link =
            KeeperLink::start_marked(sock, Identity::same_uid(), "t".into(), None, None, true)
                .expect("link");
        assert!(link.enqueue(register));
        let len = aterm_uds::shm::PAGE;
        let bad_fd = OwnedFd::from(std::fs::File::open("/dev/null").expect("/dev/null"));
        let refused_receipt = std::sync::Arc::new(CompanionReceipt::default());
        assert!(link.enqueue(LinkOp::Companion {
            rdev,
            region: bad_fd,
            len: len as u64,
            receipt: Some(std::sync::Arc::clone(&refused_receipt)),
        }));
        #[expect(
            clippy::disallowed_methods,
            reason = "a test playing the window makes the region the keeper is sent"
        )]
        // SAFETY: this fixture's link and keeper only transfer/hold/query custody, never reopen, map, write or resize it; `_good` remains inert until it is dropped.
        let (_good, good_custody) =
            unsafe { aterm_uds::shm::Region::create(len) }.expect("a region");
        let accepted_receipt = std::sync::Arc::new(CompanionReceipt::default());
        assert!(link.enqueue(LinkOp::Companion {
            rdev,
            region: good_custody,
            len: len as u64,
            receipt: Some(std::sync::Arc::clone(&accepted_receipt)),
        }));
        step_until(
            &mut server,
            "the valid replacement companion after a refusal is confirmed",
            |_| accepted_receipt.confirmed(),
        );
        assert!(
            !refused_receipt.confirmed(),
            "the refused companion's receipt is never confirmed"
        );
        assert!(
            server
                .core()
                .companion_view(rdev)
                .is_some_and(|v| v.offered.is_some()),
            "the keeper holds the accepted replacement region"
        );
        link.shutdown();
        drop(server);
    }
}

/// What a full or ended link does with an op, on any unix (no keeper is reached).
#[cfg(test)]
mod offer_tests {
    use super::*;

    #[test]
    fn a_full_queue_hands_an_offered_op_back_and_a_gone_worker_does_too() {
        let (link, far) = KeeperLink::stalled();
        for rdev in 0..LINK_QUEUE as u64 {
            assert!(link.offer(LinkOp::Release { rdev }).is_ok(), "{rdev} fits");
        }
        let marked = u64::MAX;
        match link.offer(LinkOp::CompanionRelease {
            rdev: marked,
            receipt: None,
        }) {
            Err(Refused::Full(LinkOp::CompanionRelease { rdev, .. })) => assert_eq!(rdev, marked),
            other => panic!("a full queue hands the op back: {other:?}"),
        }
        assert!(
            !link.enqueue(LinkOp::CompanionRelease {
                rdev: marked - 1,
                receipt: None
            }),
            "control: enqueue drops it"
        );
        let drained = far.drain();
        assert_eq!(drained.len(), LINK_QUEUE, "only what fit");
        assert!(
            !drained
                .iter()
                .any(|op| matches!(op, LinkOp::CompanionRelease { .. })),
            "neither the refused offer nor the dropped enqueue reached the keeper"
        );
        assert!(
            link.offer(LinkOp::CompanionRelease {
                rdev: marked,
                receipt: None
            })
            .is_ok(),
            "drained, the queue takes it"
        );
        assert!(matches!(
            far.drain().as_slice(),
            [LinkOp::CompanionRelease { rdev, .. }] if *rdev == marked
        ));
        drop(far);
        assert!(matches!(
            link.offer(LinkOp::Release { rdev: 1 }),
            Err(Refused::Gone(LinkOp::Release { rdev: 1 }))
        ));
        link.shutdown();

        let observer = std::sync::Arc::new(ReadObserver::default());
        let receipt = std::sync::Arc::new(CompanionReceipt::reporting_confirmation(
            std::sync::Arc::clone(&observer),
        ));
        let mut read = Some(CompanionRead {
            receipt: std::sync::Arc::clone(&receipt),
            carried: Vec::new(),
            fence: None,
        });
        confirm_companion(&mut read, u64::MAX);
        assert!(!receipt.confirmed() && read.is_some());
        let refused_sent = std::time::Instant::now();
        read.as_mut().expect("pending").fence = Some(CompanionFence {
            after: 6,
            until: 7,
            sent: refused_sent,
            refused: false,
        });
        note_companion_refusal(&mut read, 5);
        note_companion_refusal(&mut read, 7);
        confirm_companion(&mut read, 6);
        assert!(!receipt.confirmed() && read.is_some());
        note_companion_refusal(&mut read, 6);
        confirm_companion(&mut read, 7);
        assert!(!observer.confirmed() && observer.confirmation().is_none());
        assert!(
            !receipt.confirmed() && read.is_some(),
            "a refusal is not acceptance"
        );
        unconfirm_companion(&mut read);
        confirm_companion(&mut read, 7);
        assert!(!observer.confirmed());
        assert!(
            !receipt.confirmed() && read.is_some(),
            "the disconnected op is unsent"
        );
        let accepted_sent = refused_sent + Duration::from_nanos(1);
        read.as_mut().expect("pending").fence = Some(CompanionFence {
            after: 0,
            until: 1,
            sent: accepted_sent,
            refused: false,
        });
        confirm_companion(&mut read, 0);
        assert!(
            !receipt.confirmed() && read.is_some(),
            "reconnected but unanswered"
        );
        confirm_companion(&mut read, 1);
        assert!(receipt.confirmed() && read.is_none());
        let confirmed = observer
            .confirmation()
            .expect("the observer heard the accepted read");
        assert_eq!(
            confirmed.sync_sent, accepted_sent,
            "the observer times the SYNC whose answer confirmed the read, not a refused one's"
        );
        assert!(
            confirmed.answer_read >= refused_sent,
            "the answer is read after its SYNC"
        );
        assert!(observer.confirmed());

        let now = std::time::Instant::now();
        let make = || {
            let (answer, read) = mpsc::sync_channel(1);
            let state = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(Waited::QUEUED));
            let waited = Waited {
                answer,
                state: std::sync::Arc::clone(&state),
                until: Some(now + Duration::from_secs(60)),
            };
            ((LinkOp::Pending { pid: 7 }, Some(waited)), state, read)
        };
        let (queued, state, answer) = make();
        Waited::cancel(&state);
        assert!(claim_queued(queued, now).is_none());
        assert!(!answer.try_recv().expect("cancelled wait answered"));
        let (queued, _, answer) = make();
        assert!(claim_queued(queued, now + Duration::from_secs(60)).is_none());
        assert!(!answer.try_recv().expect("expired wait answered"));
        let (queued, state, answer) = make();
        let (op, waited) = claim_queued(queued, now).expect("live wait claimed");
        assert!(matches!(op, LinkOp::Pending { pid: 7 }));
        Waited::cancel(&state);
        assert_eq!(
            state.load(std::sync::atomic::Ordering::Acquire),
            Waited::CLAIMED,
            "a timeout cannot retract an already-running op"
        );
        waited
            .expect("waiter")
            .try_send(true)
            .expect("written answer");
        assert!(answer.try_recv().expect("live wait answered"));
    }

    #[test]
    fn a_waited_op_finds_room_in_a_queue_its_ordinary_ops_filled() {
        let (link, far) = KeeperLink::stalled();
        for rdev in 0..LINK_QUEUE as u64 {
            assert!(link.offer(LinkOp::Release { rdev }).is_ok(), "{rdev} fits");
        }
        assert!(
            matches!(
                link.offer(LinkOp::CompanionRelease {
                    rdev: 1,
                    receipt: None
                }),
                Err(Refused::Full(_))
            ),
            "control: an ordinary op finds the queue full"
        );
        let now = std::time::Instant::now();
        let waited = u32::try_from(WAITED_ROOM).expect("a few places");
        for pid in 0..=waited {
            let (answer, _read) = mpsc::sync_channel(1);
            let request = Waited {
                answer,
                state: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(Waited::QUEUED)),
                until: Some(now + Duration::from_secs(60)),
            };
            let queued = link
                .tx
                .try_send((LinkOp::Pending { pid }, Some(request)))
                .is_ok();
            assert_eq!(queued, pid < waited, "only the reserved capacity fits");
        }
        assert!(
            !link.send_and_wait(LinkOp::Pending { pid: 99 }, Duration::ZERO),
            "an already-expired nonterminal wait is never admitted"
        );
        let drained = far.drain_at(now);
        assert_eq!(drained.len(), LINK_QUEUE + WAITED_ROOM);
        assert!(
            drained[..LINK_QUEUE]
                .iter()
                .all(|op| matches!(op, LinkOp::Release { .. }))
        );
        let pids: Vec<u32> = drained[LINK_QUEUE..]
            .iter()
            .map(|op| match op {
                LinkOp::Pending { pid } => *pid,
                other => panic!("only waited ops follow: {other:?}"),
            })
            .collect();
        assert_eq!(pids, (0..waited).collect::<Vec<_>>());
        assert!(
            link.offer(LinkOp::Release { rdev: 1 }).is_ok(),
            "read, the queue takes ordinary ops again"
        );

        let (link, far) = KeeperLink::stalled();
        while link.enqueue(LinkOp::Release { rdev: 1 }) {}
        drop(far);
        assert!(
            matches!(
                link.offer(LinkOp::Release { rdev: 2 }),
                Err(Refused::Gone(_))
            ),
            "an ended worker is gone, not full"
        );
    }

    /// STATUS CHECKS THE KEEPER'S IDENTITY FLOOR AND EMPTY WELCOME: a `Designated` client
    /// calling `status()` refuses a `SameUid` or legacy untagged `WELCOME` with `NotThisBuild`
    /// and rejects a non-zero `offers` count as out of protocol.
    /// NEGATIVE CONTROL: a `Designated` `WELCOME` with `offers: 0` succeeds and records `caps`,
    /// and a `SameUid` client accepts a `SameUid` `WELCOME`.
    #[cfg(unix)]
    #[test]
    fn status_enforces_the_clients_identity_floor_and_empty_welcome() {
        let run_case = |policy: IdentityPolicy,
                        offers: u16,
                        served: Option<IdentityPolicy>,
                        caps: Option<u8>,
                        reply_status: bool| {
            let (stream, peer_stream) = CtlStream::pair().expect("pair");
            let peer = std::thread::spawn(move || {
                peer_stream
                    .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
                    .expect("timeout");
                let hello = read_frame(&peer_stream)
                    .expect("read")
                    .expect("hello")
                    .frame;
                assert!(
                    matches!(
                        hello,
                        Frame::Hello {
                            class: PeerClass::Status,
                            ..
                        }
                    ),
                    "{hello:?}"
                );
                let welcome = Frame::Welcome {
                    proto: PROTO,
                    offers,
                    version: "test".into(),
                    identity: served,
                    caps,
                };
                write_frame(&peer_stream, &welcome, None).expect("welcome");
                if reply_status {
                    let ask = read_frame(&peer_stream)
                        .expect("read")
                        .expect("status")
                        .frame;
                    assert!(matches!(ask, Frame::Status { .. }), "{ask:?}");
                    write_frame(
                        &peer_stream,
                        &Frame::Status {
                            text: "keeper=running".into(),
                        },
                        None,
                    )
                    .expect("status reply");
                    Some(peer_stream)
                } else {
                    None
                }
            });
            // This peer always answers, so its frames wait out only the hang detector.
            let wait = aterm_spec::wait::MUST_HAPPEN;
            stream.set_read_timeout(Some(wait)).expect("read timeout");
            stream.set_write_timeout(Some(wait)).expect("write timeout");
            let client = KeeperClient {
                stream,
                frame_wait: wait,
                policy,
                keeper_caps: Cell::new(0),
                syncs: Cell::new(0),
                synced: Cell::new(0),
                retiring: RefCell::new(Vec::new()),
            };
            let got = client.status("t");
            let _held = peer.join().expect("peer");
            (client, got)
        };

        let (_, uid_floor) = run_case(
            IdentityPolicy::Designated,
            0,
            Some(IdentityPolicy::SameUid),
            Some(0),
            false,
        );
        let err = uid_floor.expect_err("Designated status must refuse --identity uid keeper");
        assert!(refused_identity(&err), "{err}");
        assert!(err.to_string().contains("--identity uid"), "{err}");

        let (_, untagged) = run_case(IdentityPolicy::Designated, 0, None, None, false);
        let err = untagged.expect_err("Designated status must refuse untagged legacy keeper");
        assert!(refused_identity(&err), "{err}");

        let (_, non_empty) = run_case(
            IdentityPolicy::Designated,
            1,
            Some(IdentityPolicy::Designated),
            Some(0),
            false,
        );
        let err = non_empty.expect_err("status WELCOME with offers != 0 is out of protocol");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");

        let (designated_client, ok) = run_case(
            IdentityPolicy::Designated,
            0,
            Some(IdentityPolicy::Designated),
            Some(CAP_JOURNAL | CAP_SYNC),
            true,
        );
        assert_eq!(ok.expect("Designated keeper accepted"), "keeper=running");
        assert!(designated_client.keeper_journals() && designated_client.keeper_syncs());

        let (_, same_uid_ok) = run_case(
            IdentityPolicy::SameUid,
            0,
            Some(IdentityPolicy::SameUid),
            Some(0),
            true,
        );
        assert_eq!(
            same_uid_ok.expect("SameUid client accepted"),
            "keeper=running"
        );
    }
}
