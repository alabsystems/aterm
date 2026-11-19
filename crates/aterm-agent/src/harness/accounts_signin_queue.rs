// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One manual FIFO beside the existing machine-wide sign-in lock. Tickets supply
//! ordering, never account authorization. Callers retain their own operation scope.
//!
//! The queue is a private folder of empty ticket files, each held under a kernel file lock by
//! the process that queued it: on Unix a 0700 directory of 0600 files under `flock(2)`, on
//! Windows a folder and files under the token user's protected access list (the user and SYSTEM
//! alone, `aterm_uds::private_file`'s Windows rule) under `LockFileEx`. Either way a lock
//! released by its holder's exit is the only proof a place was abandoned: no process id, clock
//! or name is read for it.

use super::{Lock, Store};
use std::io;
use std::ops::Deref;

/// Whether this platform provides the queue's private filesystem admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManualSigninCapability {
    /// Private tickets and kernel-lock liveness are available.
    Available,
    /// Manual admission refuses; ordinary background kernel exclusion remains available.
    Unavailable,
}

/// The capability of the actual queue implementation, without filesystem effects.
#[must_use]
pub const fn manual_signin_capability() -> ManualSigninCapability {
    if cfg!(any(unix, windows)) {
        ManualSigninCapability::Available
    } else {
        ManualSigninCapability::Unavailable
    }
}

/// What currently prevents this manual ticket from acquiring the sign-in turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SigninBlocker {
    /// An earlier live manual ticket still owns its place, waiting or running.
    ManualAhead,
    /// No earlier ticket exists, but a sign-in outside this ticket holds the kernel lock.
    UnscheduledSignin,
    /// The head ticket observed the kernel lock available; this is not a reservation.
    None,
}

/// A snapshot of live FIFO ownership, not a latency estimate or sign-in permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SigninPosition {
    /// Live manual owners ahead, including an earlier owner already running.
    pub ahead: usize,
    /// The observed reason this ticket cannot enter, independently of `ahead`.
    pub blocker: SigninBlocker,
}

/// The result of one nonblocking turn attempt.
#[derive(Debug)]
pub enum SigninAttempt {
    /// The ticket retains its original place.
    Waiting(SigninPosition),
    /// The actual kernel sign-in lock and the ticket are held together.
    Acquired(SigninTurn),
}

/// Actual machine exclusion, with any manual place retained until after its release.
pub struct SigninTurn {
    signin: Lock,
    _ticket: Option<Lock>,
}

impl Deref for SigninTurn {
    type Target = Lock;
    fn deref(&self) -> &Self::Target {
        &self.signin
    }
}

impl SigninTurn {
    /// Checks that this original held signer still names this store's machine lock.
    /// # Errors
    /// The named private lock is unavailable, changed, or this platform has no FIFO.
    pub(in crate::harness) fn check_store(&self, store: &Store) -> io::Result<()> {
        #[cfg(any(unix, windows))]
        {
            fifo::check_turn(self, store)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = store;
            Err(unsupported())
        }
    }
}

/// One non-cloneable manual place. Dropping it releases liveness; a later gate holder
/// can prune its safe abandoned record. Drop performs no deletion or retirement claim.
pub struct SigninTicket {
    #[cfg(any(unix, windows))]
    store: Store,
    #[cfg(any(unix, windows))]
    root: std::path::PathBuf,
    #[cfg(any(unix, windows))]
    name: String,
    #[cfg(any(unix, windows))]
    held: Option<Lock>,
}

impl std::fmt::Debug for SigninTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SigninTicket(owned)")
    }
}

impl std::fmt::Debug for SigninTurn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SigninTurn(owned)")
    }
}

impl SigninTicket {
    /// Observe this live ticket's current position under the shared queue gate.
    /// # Errors
    /// Queue contention, missing/replaced ticket, unsafe or malformed metadata, or unsupported OS.
    pub fn position(&self) -> io::Result<SigninPosition> {
        #[cfg(any(unix, windows))]
        {
            fifo::position(self)
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(unsupported())
        }
    }

    /// Try the actual sign-in lock only at the current FIFO head, without waiting.
    /// On success this handle is consumed: later calls refuse. The returned guard must
    /// outlive actual work and its cleanup. Existing account authority checks still apply.
    /// # Errors
    /// As [`Self::position`]; a refused gate leaves this handle and its place intact.
    pub fn try_enter(&mut self) -> io::Result<SigninAttempt> {
        #[cfg(any(unix, windows))]
        {
            fifo::enter(self)
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(unsupported())
        }
    }
}

impl Store {
    /// Wait on a worker for one manual FIFO turn. `current` is checked before
    /// enqueue and every attempt; false abandons only this operation's ticket.
    /// `queued` observes position after the queue gate has been released. The
    /// existing lock ladder's first pause bounds cancellation polling, not liveness.
    /// Returns the turn and whether admission was contended, or `None` on cancellation.
    /// # Errors
    /// As [`Self::queue_manual_signin`]. Gate contention is retried without
    /// replacing an already admitted ticket. This does not validate account policy.
    pub fn manual_signin_while(
        &self,
        mut current: impl FnMut() -> bool,
        mut queued: impl FnMut(SigninPosition),
    ) -> io::Result<Option<(SigninTurn, bool)>> {
        let mut waited = false;
        let mut ticket = loop {
            if !current() {
                return Ok(None);
            }
            match self.queue_manual_signin() {
                Ok(ticket) => break ticket,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error),
            }
            waited = true;
            std::thread::sleep(super::LOCK_LADDER[0]);
        };
        loop {
            if !current() {
                return Ok(None);
            }
            match ticket.try_enter() {
                Ok(SigninAttempt::Acquired(turn)) => {
                    return if current() {
                        Ok(Some((turn, waited)))
                    } else {
                        Ok(None)
                    };
                }
                Ok(SigninAttempt::Waiting(position)) => queued(position),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error),
            }
            waited = true;
            std::thread::sleep(super::LOCK_LADDER[0]);
        }
    }

    /// Enqueue one manual sign-in in the same machine namespace every CLI pool uses.
    /// This starts no child and confers no account or credential authority.
    /// # Errors
    /// Gate contention, queue capacity/sequence exhaustion, unsafe or malformed metadata,
    /// kernel lock errors, or unsupported OS. No live ticket is returned after a failed
    /// enqueue; an abandoned record may remain for lock-proven cleanup. On Windows an
    /// elevated process makes no ticket (`aterm_uds::private_file::ELEVATED`).
    pub fn queue_manual_signin(&self) -> io::Result<SigninTicket> {
        #[cfg(any(unix, windows))]
        {
            fifo::enqueue(self)
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(unsupported())
        }
    }

    /// Try machine exclusion only when no manual ticket owns a waiting or active turn.
    /// # Errors
    /// As [`Self::queue_manual_signin`]. `None` means contention, never permission to bypass.
    pub fn try_background_signin(&self) -> io::Result<Option<SigninTurn>> {
        #[cfg(any(unix, windows))]
        {
            match fifo::background(self) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
                taken => taken,
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            self.signin_kernel_now().map(|lock| {
                lock.map(|signin| SigninTurn {
                    signin,
                    _ticket: None,
                })
            })
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "private sign-in FIFO is unavailable on this platform",
    )
}

/// The queue on the private filesystem: one rule on both platforms, with what each makes and
/// looks at in its own arm (the queue folder's parent, the gate's and a ticket's creation, the
/// machine lock's check).
#[cfg(any(unix, windows))]
mod fifo {
    use super::*;
    use aterm_uds::private_file;
    use std::fs::File;
    use std::path::{Path, PathBuf};

    const GATE: &str = "queue.lock";
    const MAX_TICKETS: usize = 256;

    /// On Unix the machine lock is judged as a private file of the store's private directory
    /// and must be the one this turn holds. On Windows the store's folder is a plain one
    /// (`aterm_types::fs_restricted` narrows no access list there), so the lock in it is no
    /// private file: what is checked is that the name is still the file this turn holds, looked
    /// at as itself (a link or junction there is another file), and nothing is made.
    pub(super) fn check_turn(turn: &SigninTurn, store: &Store) -> io::Result<()> {
        #[cfg(unix)]
        let same_lock = {
            let current =
                private_file::open_private_rw(&store.shared_dir(), super::super::SIGNIN_LOCK_FILE)
                    .map_err(judged)?;
            same(&turn.signin.0, &current)?
        };
        #[cfg(windows)]
        let same_lock = same_at(
            &turn.signin.0,
            &store.shared_dir().join(super::super::SIGNIN_LOCK_FILE),
        )?;
        if same_lock { Ok(()) } else { Err(invalid()) }
    }

    fn invalid() -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "sign-in queue metadata is unavailable or changed",
        )
    }

    fn judged(error: private_file::PrivateFileError) -> io::Error {
        io::Error::other(error.to_string())
    }

    /// The queue's private folder in the store's shared directory. On Unix that directory is
    /// itself made and checked private; on Windows it is the plain folder the store makes there,
    /// and the queue's folder alone is made private (the user and SYSTEM, protected).
    fn directory(store: &Store) -> io::Result<PathBuf> {
        let shared = store.shared_dir();
        if !shared.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "sign-in queue requires an absolute store",
            ));
        }
        aterm_types::fs_restricted::ensure_private_dir(&shared)?;
        #[cfg(unix)]
        private_file::check_private_dir(&shared)?;
        let root = shared.join("signin-queue");
        private_file::ensure_private_dir(&root)?;
        Ok(root)
    }

    fn gate(root: &Path) -> io::Result<Lock> {
        // Empty lock files need no replacement/temp-file protocol. A crash before locking
        // leaves a regular lock file, never an unclassifiable temporary queue record.
        #[cfg(unix)]
        let created = super::super::open_retained_file(&root.join(GATE), "queue gate")?;
        // On Windows the private open makes the file, under the user's protected list, where it
        // is missing: a file another open made first would take the folder's inherited entries,
        // which the private rule refuses.
        let file = private_file::open_private_rw(root, GATE).map_err(judged)?;
        #[cfg(unix)]
        {
            if file.metadata()?.len() != 0 || !same(&created, &file)? {
                return Err(invalid());
            }
            drop(created);
        }
        #[cfg(windows)]
        if file.metadata()?.len() != 0 {
            return Err(invalid());
        }
        Store::try_take(file)?
            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "sign-in queue gate is held"))
    }

    /// Whether the two open files are one file: its device and inode on Unix, its volume and
    /// 128-bit file id on Windows ([`aterm_uds::line_log::identity_of`]).
    fn same(left: &File, right: &File) -> io::Result<bool> {
        let left = aterm_uds::line_log::identity_of(left)?;
        let right = aterm_uds::line_log::identity_of(right)?;
        Ok(left.dev == right.dev && left.ino == right.ino)
    }

    /// Whether the open `file` is the file at `path` itself, a final link not followed
    /// ([`aterm_uds::line_log::identity_at`]).
    fn same_at(file: &File, path: &Path) -> io::Result<bool> {
        let held = aterm_uds::line_log::identity_of(file)?;
        let there = aterm_uds::line_log::identity_at(path)?;
        Ok(held.dev == there.dev && held.ino == there.ino)
    }

    /// A NEW ticket file, empty, never one that is there: on Unix mode 0600 and never through a
    /// link; on Windows under the token user's protected list from its creation
    /// (`CREATE_NEW`), which refuses any name there, a link or a junction included.
    fn create_ticket(root: &Path, name: &str) -> io::Result<File> {
        #[cfg(unix)]
        {
            super::super::create_retained_file(&root.join(name), "queue ticket")
        }
        #[cfg(windows)]
        {
            aterm_uds::acl::create_file(&root.join(name), &private_file::new_file_descriptor()?)
        }
    }

    fn sequence(name: &str) -> io::Result<u64> {
        let suffix = name.strip_prefix("ticket-").ok_or_else(invalid)?;
        let (sequence, nonce) = suffix.split_once('-').ok_or_else(invalid)?;
        if sequence.len() != 16
            || nonce.len() != 32
            || !sequence
                .bytes()
                .chain(nonce.bytes())
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(invalid());
        }
        let number = u64::from_str_radix(sequence, 16).map_err(|_| invalid())?;
        if number == 0 {
            return Err(invalid());
        }
        Ok(number)
    }

    // The short gate serializes every cooperative writer. Unknown entries are never skipped.
    // A successful ticket lock take is liveness evidence only, not process/child completion.
    fn live(root: &Path) -> io::Result<Vec<(u64, String)>> {
        private_file::check_private_dir(root)?;
        let mut names = Vec::new();
        for entry in std::fs::read_dir(root)? {
            let name = entry?.file_name().into_string().map_err(|_| invalid())?;
            if name == GATE {
                continue;
            }
            sequence(&name)?;
            if names.len() == MAX_TICKETS {
                return Err(invalid());
            }
            names.push(name);
        }
        let mut live = Vec::new();
        for name in names {
            let file = private_file::open_private_rw(root, &name).map_err(judged)?;
            if file.metadata()?.len() != 0 {
                return Err(invalid());
            }
            match Store::try_take(file)? {
                Ok(held) => {
                    // Refuse a swapped path; no old descriptor authorizes unlinking another file.
                    if !same_at(&held.0, &root.join(&name))? {
                        return Err(invalid());
                    }
                    // On Windows the name goes while this handle is open (every open here
                    // shares deletion), before the gate lets another reader list the folder.
                    private_file::remove_private(root, &name)?;
                    drop(held);
                }
                Err(_) => live.push((sequence(&name)?, name)),
            }
        }
        live.sort();
        if live.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err(invalid());
        }
        Ok(live)
    }

    pub(super) fn enqueue(store: &Store) -> io::Result<SigninTicket> {
        let root = directory(store)?;
        let _gate = gate(&root)?;
        let live = live(&root)?;
        if live.len() == MAX_TICKETS {
            return Err(io::Error::other("manual sign-in queue is full"));
        }
        let next = live
            .last()
            .map_or(Some(1), |(last, _)| last.checked_add(1))
            .ok_or_else(invalid)?;
        let name = format!("ticket-{next:016x}-{}", aterm_uds::rand::hex_token::<16>()?);
        let file = create_ticket(&root, &name)?;
        let held = Store::try_take(file)?.map_err(|_| invalid())?;
        let judged = private_file::open_private_rw(&root, &name).map_err(judged)?;
        if !same(&held.0, &judged)? {
            return Err(invalid());
        }
        drop(judged);
        Ok(SigninTicket {
            store: store.clone(),
            root,
            name,
            held: Some(held),
        })
    }

    fn index(ticket: &SigninTicket, live: &[(u64, String)]) -> io::Result<usize> {
        let held = ticket.held.as_ref().ok_or_else(invalid)?;
        let (file, _) = private_file::open_private(&ticket.root, &ticket.name).map_err(judged)?;
        if !same(&held.0, &file)? {
            return Err(invalid());
        }
        drop(file);
        live.iter()
            .position(|(_, name)| name == &ticket.name)
            .ok_or_else(invalid)
    }

    fn waiting(ahead: usize, held: bool) -> SigninPosition {
        SigninPosition {
            ahead,
            blocker: if ahead > 0 {
                SigninBlocker::ManualAhead
            } else if held {
                SigninBlocker::UnscheduledSignin
            } else {
                SigninBlocker::None
            },
        }
    }

    pub(super) fn position(ticket: &SigninTicket) -> io::Result<SigninPosition> {
        let _gate = gate(&ticket.root)?;
        let live = live(&ticket.root)?;
        let ahead = index(ticket, &live)?;
        if ahead > 0 {
            return Ok(waiting(ahead, true));
        }
        let available = ticket.store.signin_kernel_now()?;
        Ok(waiting(0, available.is_none()))
    }

    pub(super) fn enter(ticket: &mut SigninTicket) -> io::Result<SigninAttempt> {
        let _gate = gate(&ticket.root)?;
        let live = live(&ticket.root)?;
        let ahead = index(ticket, &live)?;
        if ahead > 0 {
            return Ok(SigninAttempt::Waiting(waiting(ahead, true)));
        }
        match ticket.store.signin_kernel_now()? {
            None => Ok(SigninAttempt::Waiting(waiting(0, true))),
            Some(signin) => Ok(SigninAttempt::Acquired(SigninTurn {
                signin,
                _ticket: ticket.held.take(),
            })),
        }
    }

    pub(super) fn background(store: &Store) -> io::Result<Option<SigninTurn>> {
        let root = directory(store)?;
        let _gate = gate(&root)?;
        if !live(&root)?.is_empty() {
            return Ok(None);
        }
        Ok(store.signin_kernel_now()?.map(|signin| SigninTurn {
            signin,
            _ticket: None,
        }))
    }
}

#[cfg(all(test, any(unix, windows)))]
#[path = "accounts_signin_queue_tests.rs"]
mod tests;
