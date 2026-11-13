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
    CAP_JOURNAL, CAP_KEEPS, CAP_LINK, CAP_SENDS_BYE, CAP_SYNC, COMPANION_JOURNAL, Frame,
    JournalOffer, MarkerRef, MasterHeader, PROTO, PeerClass, RefuseCode,
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
    /// Its last window's quit kept it past the quit (that window's BYE named it), so no crash
    /// left it on offer; `false` from a keeper that does not say (`Frame::Offer`'s `kept`).
    pub kept: bool,
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

    /// Whether the keeper's WELCOME stated it honours a BYE's keep set ([`CAP_KEEPS`]).
    #[must_use]
    pub const fn keeper_keeps(&self) -> bool {
        self.keeper_caps.get() & CAP_KEEPS != 0
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
                caps: CAP_SENDS_BYE | CAP_KEEPS | if journal { CAP_JOURNAL } else { 0 },
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
            let (master, header, tag, meta, trail, kept) = match self.recv()? {
                Some(Incoming {
                    frame:
                        Frame::Offer {
                            header,
                            tag,
                            meta,
                            journal: trail,
                            kept,
                        },
                    fd: Some(master),
                }) => (master, header, tag, meta, trail, kept.unwrap_or(false)),
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
                kept,
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
    /// The quit, written on the final-exit path just before `exit(0)`, naming the masters whose
    /// shells go on past it for the next window to adopt (`keep`, honoured only by a keeper that
    /// states [`CAP_KEEPS`]: [`KeeperLink::keeper_keeps`]).
    Bye { keep: Vec<u64> },
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
    /// What the keeper the worker's connection reached holds as the worker last said it
    /// ([`Self::keeper_keeps`], [`Self::keeps`]).
    custody: std::sync::Arc<Custody>,
}

/// What the keeper the link's worker reached holds, as the worker says it at each turn and
/// reconnect ([`KeeperLink::keeps`]): nothing while it has no connection, and once it has ended.
#[derive(Debug, Default)]
struct Custody {
    /// That keeper's WELCOME caps, 0 with none.
    caps: std::sync::atomic::AtomicU8,
    /// The masters whose REGISTER that connection carried, with no refusal of them read since.
    masters: std::sync::Mutex<std::collections::BTreeSet<u64>>,
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
        let custody = std::sync::Arc::new(Custody {
            caps: std::sync::atomic::AtomicU8::new(
                booted.as_ref().map_or(0, |c| c.keeper_caps.get()),
            ),
            ..Custody::default()
        });
        let stated = Stated(std::sync::Arc::clone(&custody));
        let thread = std::thread::Builder::new()
            .name("aterm-keeper-link".to_string())
            .spawn(move || {
                let ended = ended;
                link_worker(&peer, rx, door, booted, &ended.0, &stated);
            })?;
        Ok(Self {
            tx,
            room,
            bell,
            thread: Some(thread),
            custody,
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
                custody: std::sync::Arc::default(),
            },
            Stalled {
                rx: std::sync::Mutex::new(rx),
                room,
                _door: door,
            },
        )
    }

    /// Whether the keeper the worker's connection reached now stated it honours a BYE's keep set
    /// ([`CAP_KEEPS`]): `false` while the worker has no connection, and once it has ended. A
    /// reconnect to another keeper between this read and the BYE leaves the keep set to that
    /// keeper, which an older one reads as a plain BYE.
    #[must_use]
    pub fn keeper_keeps(&self) -> bool {
        self.custody.caps.load(std::sync::atomic::Ordering::Acquire) & CAP_KEEPS != 0
    }

    /// Whether a BYE written now would keep master `rdev` past the quit: the keeper the worker's
    /// connection reached keeps what a BYE names ([`Self::keeper_keeps`]), and that connection
    /// carried the master's REGISTER, which the keeper reads before anything written after it,
    /// with no refusal of it read since. `false` for a master whose REGISTER is still queued, or
    /// owed behind a full queue (a BYE drops what is owed). A reconnect registers every held
    /// master again before anything else, so it keeps this true; a refusal still in flight when
    /// the BYE goes is the one thing it cannot see.
    #[must_use]
    pub fn keeps(&self, rdev: u64) -> bool {
        self.keeper_keeps()
            && self
                .custody
                .masters
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains(&rdev)
    }

    /// Queue `op` using waited room and wait for a written answer.
    pub fn send_and_wait(&self, op: LinkOp, within: Duration) -> bool {
        let lasting = matches!(op, LinkOp::Bye { .. });
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
    stated: &Stated,
) {
    let mut held: Held = std::collections::BTreeMap::new();
    let mut metas: std::collections::BTreeMap<u64, Vec<u8>> = std::collections::BTreeMap::new();
    let mut owed: Owed = Owed::new();
    // The boot connection's retirements ([`KeeperClient::retire`]) are this link's to see through.
    for (rdev, owe) in booted.iter().flat_map(|c| c.retiring.take()) {
        keep(&mut owed, rdev, owe);
    }
    let mut client: Option<KeeperClient> = booted;
    // The held masters whose REGISTER the keeper refused on this connection.
    let mut refused: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
    let mut backoff = Duration::from_millis(50);
    let mut next_try = std::time::Instant::now();
    loop {
        stated.now(client.as_ref(), &held, &refused);
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
                // Every refusal but a region's says the keeper holds no claim of this
                // connection's on the master.
                if code != RefuseCode::NotAJournal && held.contains_key(&rdev) {
                    refused.insert(rdev);
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
                refused.clear();
                unconfirm(&mut owed);
                unconfirm_companions(&mut held);
            }
        }
        let bye = matches!(op, Some(LinkOp::Bye { .. }));
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
                refused.remove(&header.rdev);
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
                refused.remove(&rdev);
                let registered = held.remove(&rdev);
                let send = registered.is_some() || client.is_some();
                if let Some(h) = registered {
                    must_reach = Some((rdev, Owe::releasing(h)));
                }
                send.then_some(Send::Frame(Frame::Release { rdev }))
            }
            LinkOp::Retire { rdev, observer } => {
                metas.remove(&rdev);
                refused.remove(&rdev);
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
            LinkOp::Bye { keep } => Some(Send::Frame(Frame::Bye { keep })),
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
            refused.clear();
            stated.now(client.as_ref(), &held, &refused);
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
        if sent.is_err() {
            client = None;
            refused.clear();
            unconfirm(&mut owed);
            unconfirm_companions(&mut held);
            owe(&mut owed, must_reach);
        }
        // Said before the answer: a caller told its REGISTER was written reads its custody.
        stated.now(client.as_ref(), &held, &refused);
        if let Some(ack) = ack {
            let _ = ack.try_send(sent.is_ok());
        }
        if bye {
            return;
        }
    }
}

/// The worker's half of [`KeeperLink::keeper_keeps`] and [`KeeperLink::keeps`]: the caps of the
/// keeper its connection reached and the masters that connection holds, written at each turn,
/// before the worker waits, and at each reconnect, and nothing once the worker ends.
struct Stated(std::sync::Arc<Custody>);

impl Stated {
    /// With `client` the worker's connection, every master of `held` but the `refused` is that
    /// keeper's: a connection carried each REGISTER of `held` (a reconnect registers them all
    /// first, and a REGISTER whose write fails ends the connection).
    fn now(
        &self,
        client: Option<&KeeperClient>,
        held: &Held,
        refused: &std::collections::BTreeSet<u64>,
    ) {
        self.0.caps.store(
            client.map_or(0, |c| c.keeper_caps.get()),
            std::sync::atomic::Ordering::Release,
        );
        let mut masters = self
            .0
            .masters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        masters.clear();
        if client.is_some() {
            masters.extend(held.keys().filter(|rdev| !refused.contains(rdev)));
        }
    }
}

impl Drop for Stated {
    fn drop(&mut self) {
        self.0.caps.store(0, std::sync::atomic::Ordering::Release);
        self.0
            .masters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
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
#[path = "client_tests.rs"]
mod tests;

/// What a full or ended link does with an op, on any unix (no keeper is reached).
#[cfg(test)]
#[path = "client_offer_tests.rs"]
mod offer_tests;
