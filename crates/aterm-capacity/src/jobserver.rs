// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One GNU FIFO jobserver for a release's branches (`docs/DESIGN-fast-build-release-2026-11-05.md`,
//! A11 and B4). Each client reserves its implicit token before spawning; Cargo and rustc borrow
//! every additional token from this FIFO.
//!
//! THE POOL AND ITS BRANCHES. The release makes the pool once ([`Jobserver::new`], `S` tokens for
//! its Release core claim of `S`) and hands it to each branch child ([`Jobserver::hand_to`], or a
//! share of fewer slots with [`Jobserver::hand_share_to`]: [`POOL_ENV`] is `<slots>:<absolute FIFO
//! path>`, the FIFO always by the pool's own path). A branch adopts it ([`Jobserver::inherited`])
//! and charges every client it starts from it: a Cargo through [`Jobserver::attach_cargo`] (its
//! implicit token, and the GNU auth its rustc workers draw the rest through), any other process
//! through [`Jobserver::acquire_many`] (its whole charge). So the jobs running across every branch
//! never hold more than `S` tokens, and only the pool's creator removes the FIFO.

#[cfg(unix)]
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
#[cfg(unix)]
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::Condvar;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

/// The variable a release hands its branch children its pool in: `<slots>:<absolute FIFO path>`,
/// the slots a decimal count in `1..=4096` and the path free of whitespace, as
/// [`Jobserver::new`] requires ([`Jobserver::hand_to`], [`Jobserver::inherited`]).
pub const POOL_ENV: &str = "ATERM_RELEASE_POOL";

/// The most tokens a pool holds.
const MAX_SLOTS: u32 = 4096;

/// How long a wait for a token goes before it asks its stop predicate again
/// ([`Jobserver::acquire`]). It never delays a token: the write that brings one ends the wait's
/// read, and a token this handle returns is handed to the wait at once.
const WAKE_EVERY: Duration = Duration::from_millis(100);

/// How soon a wait whose read must end (its caller stopped, or a token was handed to it) sends
/// its reader [`INTERRUPT`] again while the read has not ended: a signal that lands before the
/// reader is in `read(2)` ends nothing.
#[cfg(unix)]
const NUDGE_EVERY: Duration = Duration::from_millis(1);

/// The signal a wait ends its reader's `read(2)` with ([`Jobserver::acquire`]): `SIGURG`, whose
/// default action discards it and which nothing else in these processes sends or catches, routed
/// once per process to a handler that does nothing, without `SA_RESTART`, so the read returns
/// `EINTR` ([`interruptible`]).
#[cfg(unix)]
const INTERRUPT: libc::c_int = libc::SIGURG;

#[cfg(unix)]
unsafe extern "C" {
    fn pthread_self() -> libc::pthread_t;
}

/// A release-local pool, kept alive until all branch guards have been returned.
#[derive(Debug)]
pub struct Jobserver {
    path: PathBuf,
    /// The FIFO, nonblocking: [`Self::try_acquire`]'s reads and every token's return.
    file: File,
    /// The FIFO again, by its own open file description, blocking: the descriptor a wait's
    /// reader blocks in `read(2)` on ([`Self::acquire`]).
    #[cfg(unix)]
    wait: File,
    slots: u32,
    /// Whether this handle made the FIFO ([`Self::new`]), and so removes it when dropped; a
    /// handle adopted from [`POOL_ENV`] leaves it to its creator.
    created: bool,
    /// Held across [`Self::acquire_many`]: this handle's one partial holder.
    partial: Mutex<()>,
    /// This handle's waits for a token now ([`Self::acquire`]).
    #[cfg(unix)]
    waiters: Mutex<Waiters>,
    /// Told when a wait's read ends or a token is handed to a wait.
    #[cfg(unix)]
    turn: Condvar,
}

/// The waits for a token on one handle, oldest first.
#[cfg(unix)]
#[derive(Debug, Default)]
struct Waiters {
    /// The id the next wait takes; ids rise, so the map's order is the order they began in.
    next: u64,
    each: BTreeMap<u64, Waiter>,
}

/// One wait for a token ([`Jobserver::acquire`]).
#[cfg(unix)]
#[derive(Debug)]
struct Waiter {
    /// Where its reader's `read(2)` is.
    reader: Reader,
    /// A token this handle returned while it waited, handed to it instead of the FIFO
    /// ([`Jobserver::give`]).
    handed: Option<u8>,
}

/// A wait's reader thread, which blocks in `read(2)` on the pool's blocking descriptor.
#[cfg(unix)]
#[derive(Debug)]
enum Reader {
    /// Spawned, and not yet in its read.
    Starting,
    /// In its read, or about to enter it, as this thread, with [`INTERRUPT`] unblocked in its
    /// mask ([`unblock_interrupt`]).
    Reading(libc::pthread_t),
    /// Its read ended: the token it took, none when a signal ended it, or why it failed (its
    /// mask could not be changed, and it never read).
    Done(io::Result<Option<u8>>),
}

impl Jobserver {
    /// Create a new pool in a private release directory. Existing paths are never replaced.
    /// # Errors
    /// The token count or FIFO path cannot be represented, or creating/opening it fails.
    pub fn new(path: &Path, slots: u32) -> io::Result<Self> {
        representable(path, slots)?;
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            // A child in another directory adopts the pool by this path ([`Self::hand_to`]),
            // which the working directory it was made absolute in must not break.
            let path = std::path::absolute(path)?;
            representable(&path, slots)?;
            let name = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "FIFO path contains NUL")
            })?;
            // SAFETY: name is NUL terminated and valid for the duration of mkfifo.
            if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let opened = open(&path).and_then(|mut file| {
                let wait = blocking_twin(&path, &file)?;
                file.write_all(&[b'+'; MAX_SLOTS as usize][..slots as usize])?;
                Ok((file, wait))
            });
            let (file, wait) = match opened {
                Ok(both) => both,
                Err(error) => {
                    let _ = std::fs::remove_file(&path);
                    return Err(error);
                }
            };
            Ok(Self::holding(path, file, wait, slots, true))
        }
        #[cfg(not(unix))]
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "FIFO jobservers require Unix",
        ))
    }

    /// The pool a release handed this process ([`POOL_ENV`], [`Self::hand_to`]): `None` when the
    /// variable is unset, and an error when it is set but malformed or names no FIFO this user
    /// owns ([`Self::adopt`]).
    #[must_use]
    pub fn inherited() -> Option<io::Result<Self>> {
        std::env::var_os(POOL_ENV).map(|value| Self::adopt(&value))
    }

    /// Adopt the pool `value` names, in [`POOL_ENV`]'s form `<slots>:<absolute FIFO path>`. The
    /// FIFO is opened as [`Self::new`] opens it, and is never removed by this handle: the pool
    /// is its creator's. No token is taken or added.
    /// # Errors
    /// `value` is not of that form, or the path does not open as a FIFO this user owns.
    pub fn adopt(value: &OsStr) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
            let malformed = || {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{POOL_ENV} must be <slots>:<absolute FIFO path>, not {value:?}"),
                )
            };
            let bytes = value.as_bytes();
            let colon = bytes
                .iter()
                .position(|&b| b == b':')
                .ok_or_else(malformed)?;
            let (digits, rest) = (&bytes[..colon], &bytes[colon + 1..]);
            if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
                return Err(malformed());
            }
            let slots = std::str::from_utf8(digits)
                .ok()
                .and_then(|s| s.parse::<u32>().ok())
                .ok_or_else(malformed)?;
            let path = Path::new(OsStr::from_bytes(rest));
            if !path.is_absolute() || rest.contains(&0) {
                return Err(malformed());
            }
            representable(path, slots)?;
            let file = open(path)?;
            let metadata = file.metadata()?;
            // SAFETY: geteuid has no arguments and cannot fail.
            let me = unsafe { libc::geteuid() };
            if !metadata.file_type().is_fifo() || metadata.uid() != me {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "{POOL_ENV} names {}, which is not a FIFO this user owns",
                        path.display()
                    ),
                ));
            }
            let wait = blocking_twin(path, &file)?;
            Ok(Self::holding(path.to_owned(), file, wait, slots, false))
        }
        #[cfg(not(unix))]
        {
            let _ = value;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "FIFO jobservers require Unix",
            ))
        }
    }

    /// A handle on the FIFO at `path`, opened both ways ([`open`], [`blocking_twin`]), with no
    /// wait yet.
    #[cfg(unix)]
    fn holding(path: PathBuf, file: File, wait: File, slots: u32, created: bool) -> Self {
        Self {
            path,
            file,
            wait,
            slots,
            created,
            partial: Mutex::new(()),
            waiters: Mutex::default(),
            turn: Condvar::new(),
        }
    }

    /// The Release grant this pool bounds.
    #[must_use]
    pub const fn slots(&self) -> u32 {
        self.slots
    }

    /// The FIFO this pool's tokens are in, by the one absolute path every child of this handle is
    /// handed ([`Self::handed`]): [`Self::new`]'s path made absolute, or the path adopted.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// [`POOL_ENV`]'s value for this pool: `<slots>:<absolute FIFO path>`.
    #[must_use]
    pub fn handed(&self) -> OsString {
        self.value(self.slots)
    }

    /// [`POOL_ENV`]'s value for a share of this pool, `slots` of its tokens: [`Self::handed`]'s
    /// form with that count and this pool's own FIFO path ([`Self::path`]), so a child handed a
    /// share and one handed the whole pool are handed one spelling of one FIFO. A child that
    /// adopts it reads the share as its pool's slots and sizes its charges to them.
    /// # Errors
    /// `slots` is 0, which [`Self::adopt`] refuses, or more than [`Self::slots`], which would let
    /// the child wait for tokens the pool does not have.
    pub fn handed_share(&self, slots: u32) -> io::Result<OsString> {
        if slots == 0 || slots > self.slots {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "a share of a pool of {} slots is 1..={} of them, not {slots}",
                    self.slots, self.slots
                ),
            ));
        }
        Ok(self.value(slots))
    }

    /// `<slots>:<absolute FIFO path>` for this pool's FIFO.
    fn value(&self, slots: u32) -> OsString {
        let mut value = OsString::from(format!("{slots}:"));
        value.push(self.path.as_os_str());
        value
    }

    /// Hand this pool to a branch child: [`POOL_ENV`] alone, which the child adopts
    /// ([`Self::inherited`]) to charge each client it starts. No token is taken for the child and
    /// no `MAKEFLAGS` is set: a branch is a coordinator of many clients, not one GNU client. With
    /// the GNU auth in its environment every Cargo it started without [`Self::attach_cargo`]
    /// would inherit the pool and run its first job on an implicit token nobody took (each one
    /// over the grant by a job), and the test binaries it runs would read it too. A child that
    /// is itself one serial client (an engine whose gate runs one Cargo at a time) is charged by
    /// its parent instead, through tokens the parent holds while it runs.
    pub fn hand_to(&self, command: &mut aterm_uds::spawnfd::Command) {
        command.env(POOL_ENV, self.handed());
    }

    /// [`Self::hand_to`] with a share of the pool, `slots` of its tokens ([`Self::handed_share`]),
    /// for a branch child that must leave the rest to another holder of the pool.
    /// # Errors
    /// As [`Self::handed_share`]; `command` is left as it was.
    pub fn hand_share_to(
        &self,
        command: &mut aterm_uds::spawnfd::Command,
        slots: u32,
    ) -> io::Result<()> {
        command.env(POOL_ENV, self.handed_share(slots)?);
        Ok(())
    }

    /// GNU make 4.4 authentication shared by Cargo, rustc and make children.
    #[must_use]
    pub fn flags(&self) -> String {
        format!("-j --jobserver-auth=fifo:{}", self.path.display())
    }

    /// Attach Cargo to this pool and charge its implicit job before it is spawned.
    /// # Errors
    /// Acquiring the implicit token fails or the caller ends the wait.
    pub fn attach_cargo(
        &self,
        command: &mut aterm_uds::spawnfd::Command,
        stopped: &dyn Fn() -> bool,
    ) -> io::Result<Token<'_>> {
        let token = self.acquire(stopped)?;
        let flags = self.flags();
        command
            .env("MAKEFLAGS", &flags)
            .env("CARGO_MAKEFLAGS", flags)
            .arg("-j")
            .arg(self.slots.to_string());
        Ok(token)
    }

    /// Reserve a client's implicit token. The caller must retain the guard until that
    /// client's process group has drained, including on failure.
    ///
    /// FAIR AGAINST BLOCKED READERS. Cargo's and rustc's jobserver helpers wait in a blocking
    /// `read(2)` on their own descriptors of the FIFO, and the kernel gives each byte written to
    /// one of the readers blocked there, on Darwin 27.2 mostly the one that has slept longest. A
    /// wait in `poll(2)` or `kevent(2)` is woken by the same write but almost never takes the
    /// byte: it must return from the poll and read again, and the blocked reader has taken it in
    /// the kernel by then (2 in 2000 races against one blocked reader, as
    /// `docs/measured/jobserver-blocked-reader-fairness-2026-11-06.md` records). So
    /// this wait is itself a blocked reader: a thread of its own blocks in `read(2)` on the
    /// pool's blocking descriptor ([`Self::wait_for_token`]) and contends for each byte written
    /// as one more helper (about half the races against one), while the caller's thread asks
    /// `stopped` every [`WAKE_EVERY`] and, once it answers yes, ends the read with
    /// [`INTERRUPT`]. And a token this handle returns while one of its waits waits is handed to
    /// the oldest such wait instead of the FIFO ([`Self::give`]; a wait already ending gives it
    /// on once its read has ended), as Cargo keeps a finished job's token for its next queued
    /// job. Without that, each token a release client returned would go to whichever helper had
    /// slept longest, and the handle would hold only what it won fresh: in a simulation (the
    /// same record) of 8 slots, 8 Cargo-like helpers and 4 release workers (a fair share 33%),
    /// the release held 16% with a blocked reader alone, 31% with the handoff too, and 5% in
    /// `poll(2)`.
    ///
    /// LIMIT: `SIGURG` keeps the handler [`interruptible`] gives it for the life of the process,
    /// so a `SIGURG` sent to the process, which nothing sends these processes, would end one
    /// thread's blocking call with `EINTR` instead of being discarded. And its disposition is
    /// read once per process: had another part of the process later given `SIGURG` a handler
    /// with `SA_RESTART`, or `SIG_IGN` or `SIG_DFL`, a wait whose read must end (stopped, or
    /// handed a token) would signal its reader until some write ended the read, asking
    /// `stopped` no more meanwhile, and a handed wait holds the very token such a write could
    /// be. Nothing in the tree touches `SIGURG` in a process that waits here.
    /// # Errors
    /// FIFO IO fails, the wait's reader cannot be started or cannot unblock `SIGURG` in its own
    /// mask, `SIGURG` already has another handler in this process, or the caller ends the wait,
    /// even when a token reaches the wait after the stop (STOPPED BEFORE EVERY TOKEN at
    /// `acquire_waking`).
    pub fn acquire(&self, stopped: &dyn Fn() -> bool) -> io::Result<Token<'_>> {
        self.acquire_waking(stopped, WAKE_EVERY, &|| {})
    }

    /// [`Self::acquire`], asking `stopped` at least once every `wake`, and telling `waiting` each
    /// time it is about to wait for at most `wake`: once as it begins a wait, and again each
    /// `wake` it waits on (a test's hook).
    ///
    /// STOPPED BEFORE EVERY TOKEN. A token is returned only just after `stopped` said no: one
    /// found in the FIFO is read right after that answer, and one a wait got, which may have
    /// come up to `wake` after the caller stopped, waits for the next answer. A token that
    /// arrives after a stop is given back as it drops ([`Token`]'s drop). So a caller whose stop
    /// came before its token, and still holds, gets the stop's error and not the token, as when
    /// the wait was a loop of nonblocking reads.
    fn acquire_waking(
        &self,
        stopped: &dyn Fn() -> bool,
        wake: Duration,
        waiting: &dyn Fn(),
    ) -> io::Result<Token<'_>> {
        let mut won = None;
        loop {
            if stopped() {
                drop(won);
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "jobserver wait ended",
                ));
            }
            if let Some(token) = won.take() {
                return Ok(token);
            }
            if let Some(token) = self.try_acquire()? {
                return Ok(token);
            }
            won = self
                .wait_for_token(stopped, wake, waiting)?
                .map(|byte| Token { pool: self, byte });
        }
    }

    /// One wait for a token, as one of this handle's [`Waiters`]: a reader thread of the wait's
    /// own blocks in `read(2)` on the blocking descriptor, and this thread waits for its read to
    /// end or a token to be handed to it ([`Self::give`]), asking `stopped` each `wake`. A stop
    /// or a handed token ends the read with [`INTERRUPT`], sent again each [`NUDGE_EVERY`] until
    /// it has ended. The token read or handed; a token read beside a handed one is given on
    /// ([`Self::give`]), and so is any a stopped wait got, which answers `None`, as a read
    /// another signal ended does. The reader has ended, and the wait has left [`Waiters`],
    /// before this returns, so a stopped wait leaves nothing reading the FIFO. A panic in
    /// `stopped` or `waiting` ends the wait as a stop does, then goes on.
    #[cfg(unix)]
    fn wait_for_token(
        &self,
        stopped: &dyn Fn() -> bool,
        wake: Duration,
        waiting: &dyn Fn(),
    ) -> io::Result<Option<u8>> {
        use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
        interruptible()?;
        let id = {
            let mut waiters = self.waiters.lock().unwrap_or_else(PoisonError::into_inner);
            let id = waiters.next;
            waiters.next += 1;
            waiters.each.insert(
                id,
                Waiter {
                    reader: Reader::Starting,
                    handed: None,
                },
            );
            id
        };
        let mut panicked = catch_unwind(AssertUnwindSafe(waiting)).err();
        let outcome = std::thread::scope(|scope| {
            let spawned = match panicked {
                Some(_) => None,
                None => Some(
                    std::thread::Builder::new()
                        .name("jobserver wait".to_owned())
                        .spawn_scoped(scope, || self.read_blocked(id)),
                ),
            };
            let mut waiters = self.waiters.lock().unwrap_or_else(PoisonError::into_inner);
            if !matches!(spawned, Some(Ok(_))) {
                if let Some(Waiter {
                    handed: Some(byte), ..
                }) = waiters.each.remove(&id)
                {
                    self.give_locked(&mut waiters, byte);
                }
                return match spawned {
                    Some(Err(error)) => Err(error),
                    _ => Ok(None),
                };
            }
            let (mut ending, mut stopping) = (false, false);
            let mut asked = Instant::now();
            // Only this wait removes its own record.
            while let Some(me) = waiters.each.get(&id) {
                ending |= me.handed.is_some();
                match me.reader {
                    Reader::Done(_) => break,
                    Reader::Reading(thread) if ending => {
                        // SAFETY: a reader sets `Done` under this lock before its thread
                        // ends, so one seen `Reading` under it names a live thread, and
                        // INTERRUPT has the handler `interruptible` gave it; that thread
                        // unblocked INTERRUPT before it said `Reading`, so it is delivered.
                        unsafe { libc::pthread_kill(thread, INTERRUPT) };
                    }
                    Reader::Starting | Reader::Reading(_) => {}
                }
                let pause = if ending {
                    NUDGE_EVERY
                } else {
                    wake.saturating_sub(asked.elapsed())
                };
                waiters = self
                    .turn
                    .wait_timeout(waiters, pause)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
                if !ending && asked.elapsed() >= wake {
                    drop(waiters);
                    match catch_unwind(AssertUnwindSafe(stopped)) {
                        Ok(true) => stopping = true,
                        Ok(false) => panicked = catch_unwind(AssertUnwindSafe(waiting)).err(),
                        Err(panic) => panicked = Some(panic),
                    }
                    stopping |= panicked.is_some();
                    ending = stopping;
                    asked = Instant::now();
                    waiters = self.waiters.lock().unwrap_or_else(PoisonError::into_inner);
                }
            }
            let Some(Waiter {
                reader: Reader::Done(read),
                handed,
            }) = waiters.each.remove(&id)
            else {
                // Never: the loop ends only on this wait's reader being done.
                return Err(io::Error::other("jobserver wait lost its reader"));
            };
            let mut got = handed;
            let read = read.map(|byte| match (byte, got) {
                (Some(byte), None) => got = Some(byte),
                (Some(extra), Some(_)) => self.give_locked(&mut waiters, extra),
                (None, _) => {}
            });
            if stopping && let Some(byte) = got.take() {
                self.give_locked(&mut waiters, byte);
            }
            match (got, read) {
                (Some(byte), _) => Ok(Some(byte)),
                (None, read) => read.map(|()| None),
            }
        });
        if let Some(panic) = panicked {
            resume_unwind(panic);
        }
        outcome
    }

    #[cfg(not(unix))]
    fn wait_for_token(
        &self,
        _stopped: &dyn Fn() -> bool,
        _wake: Duration,
        _waiting: &dyn Fn(),
    ) -> io::Result<Option<u8>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "FIFO jobservers require Unix",
        ))
    }

    /// The reader thread of wait `id` ([`Self::wait_for_token`]): [`INTERRUPT`] unblocked in its
    /// own mask ([`unblock_interrupt`]), then one blocking `read(2)` of a token, its outcome left
    /// in the wait's [`Reader`]. A signal ends it with no token. A reader whose mask cannot be
    /// changed ends with that error and never reads, so no reader blocks with [`INTERRUPT`]
    /// masked.
    #[cfg(unix)]
    fn read_blocked(&self, id: u64) {
        let read = unblock_interrupt().and_then(|()| {
            if let Some(me) = self
                .waiters
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .each
                .get_mut(&id)
            {
                // SAFETY: pthread_self has no preconditions.
                me.reader = Reader::Reading(unsafe { pthread_self() });
            }
            let mut byte = [0];
            match (&self.wait).read(&mut byte) {
                Ok(1) => Ok(Some(byte[0])),
                Ok(_) => Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "jobserver FIFO closed",
                )),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(None),
                Err(error) => Err(error),
            }
        });
        let mut waiters = self.waiters.lock().unwrap_or_else(PoisonError::into_inner);
        match waiters.each.get_mut(&id) {
            Some(me) => me.reader = Reader::Done(read),
            None => {
                if let Ok(Some(byte)) = read {
                    self.give_locked(&mut waiters, byte);
                }
            }
        }
        self.turn.notify_all();
    }

    /// Return a token this handle held: to its oldest wait that holds none yet (neither handed
    /// one nor done reading one), or, with none, to the FIFO.
    fn give(&self, byte: u8) {
        #[cfg(unix)]
        {
            let mut waiters = self.waiters.lock().unwrap_or_else(PoisonError::into_inner);
            self.give_locked(&mut waiters, byte);
        }
        #[cfg(not(unix))]
        {
            let _ = (&self.file).write_all(&[byte]);
        }
    }

    /// [`Self::give`] under the lock already held. A return the FIFO refuses loses that token,
    /// as a token's return always could.
    #[cfg(unix)]
    fn give_locked(&self, waiters: &mut Waiters, byte: u8) {
        let holds_none =
            |w: &&mut Waiter| w.handed.is_none() && !matches!(w.reader, Reader::Done(Ok(Some(_))));
        match waiters.each.values_mut().find(holds_none) {
            Some(oldest) => {
                oldest.handed = Some(byte);
                self.turn.notify_all();
            }
            None => {
                let _ = (&self.file).write_all(&[byte]);
            }
        }
    }

    /// Take `n` tokens, at most the pool's slots, for one process that is no GNU client (a test
    /// binary, whose threads and children run on its charge), returned when the guards drop.
    ///
    /// ONE PARTIAL HOLDER. This handle's callers take their charges one at a time: a second
    /// waits on the first's lock while the first, holding part of its charge, waits for the
    /// rest. Two partial holds that each wait on what the other holds would deadlock once the
    /// FIFO ran dry; one cannot, since every other holder of the pool holds whole charges for
    /// work that ends (Cargo runs each job on a token it already holds), so the tokens the one
    /// waiter needs keep coming back; one this handle's other holders return goes to the
    /// handle's oldest wait ([`Self::give`]), the partial holder's or one for a whole charge of
    /// work that ends. Across processes the same holds while at most one process
    /// waits holding part of a charge: the release takes a branch's fixed reservation at once,
    /// before any branch starts, and a build's Cargo clients take one token each.
    /// # Errors
    /// FIFO IO fails, or the caller ends the wait; the tokens taken so far are returned.
    pub fn acquire_many(&self, n: u32, stopped: &dyn Fn() -> bool) -> io::Result<Vec<Token<'_>>> {
        let n = n.min(self.slots) as usize;
        let _one = self.partial.lock().unwrap_or_else(PoisonError::into_inner);
        let mut held = Vec::with_capacity(n);
        while held.len() < n {
            held.push(self.acquire(stopped)?);
        }
        Ok(held)
    }

    /// Take a token only if one is currently available.
    /// # Errors
    /// FIFO IO fails.
    pub fn try_acquire(&self) -> io::Result<Option<Token<'_>>> {
        let mut byte = [0];
        match (&self.file).read(&mut byte) {
            Ok(1) => Ok(Some(Token {
                pool: self,
                byte: byte[0],
            })),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
            _ => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "jobserver FIFO closed",
            )),
        }
    }
}

/// Refuse a token count or a FIFO path a pool cannot carry: 1..=4096 slots, no whitespace in the
/// path (the GNU auth string is split on it).
fn representable(path: &Path, slots: u32) -> io::Result<()> {
    if slots == 0
        || slots > MAX_SLOTS
        || path
            .as_os_str()
            .as_encoded_bytes()
            .iter()
            .any(u8::is_ascii_whitespace)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "jobserver requires 1..=4096 slots and a path without whitespace",
        ));
    }
    Ok(())
}

/// Open the FIFO for tokens both ways, nonblocking, out of a concurrent spawn's descriptors.
#[cfg(unix)]
fn open(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    aterm_uds::kept_out(|| {
        OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(path)
    })
}

/// A second, blocking descriptor of the FIFO `file` holds, by its own open file description (a
/// wait's reader blocks on it while [`Jobserver::try_acquire`] stays nonblocking on `file`). It
/// is opened as [`open`] opens, so the open itself never blocks, then refused unless it is the
/// file `file` is, and only then made blocking.
/// # Errors
/// The open fails, `path` names another file by now, or the flag cannot be cleared.
#[cfg(unix)]
fn blocking_twin(path: &Path, file: &File) -> io::Result<File> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::MetadataExt as _;
    let wait = open(path)?;
    let (first, again) = (file.metadata()?, wait.metadata()?);
    if (first.dev(), first.ino()) != (again.dev(), again.ino()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is no longer the FIFO first opened", path.display()),
        ));
    }
    let fd = wait.as_raw_fd();
    // SAFETY: `fd` is `wait`'s open descriptor; F_GETFL reads its status flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    // SAFETY: as above; F_SETFL writes them back without O_NONBLOCK.
    if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(wait)
}

/// Unblock [`INTERRUPT`] in the calling thread's signal mask, so a `pthread_kill` of this thread
/// with it is delivered at once. A wait's reader calls this before it says it is reading
/// ([`Jobserver::read_blocked`]), because a thread starts with its creator's mask and a process
/// with its parent's across `fork` and `exec`. A caller that blocks `SIGURG`, or a supervisor
/// that blocked every signal before it started the process, would otherwise leave each signal
/// pending and the read blocked: neither a stop nor a handed token could end the wait. Only this
/// thread's mask changes; the caller's keeps whatever it blocks.
/// # Errors
/// `pthread_sigmask` refuses.
#[cfg(unix)]
fn unblock_interrupt() -> io::Result<()> {
    let mut only = std::mem::MaybeUninit::<libc::sigset_t>::zeroed();
    // SAFETY: sigemptyset initialises the set it is handed, and sigaddset adds a valid signal to
    // that initialised set.
    let only = unsafe {
        libc::sigemptyset(only.as_mut_ptr());
        libc::sigaddset(only.as_mut_ptr(), INTERRUPT);
        only.assume_init()
    };
    // SAFETY: changes only the calling thread's mask, from an initialised set; the old mask is
    // not asked for.
    match unsafe { libc::pthread_sigmask(libc::SIG_UNBLOCK, &raw const only, std::ptr::null_mut()) }
    {
        0 => Ok(()),
        error => Err(io::Error::from_raw_os_error(error)),
    }
}

/// The handler [`INTERRUPT`] runs: nothing. Its one effect is that a `read(2)` it lands in
/// returns `EINTR`.
#[cfg(unix)]
extern "C" fn on_interrupt(_: libc::c_int) {}

/// Route [`INTERRUPT`] to [`on_interrupt`] without `SA_RESTART`, once per process, so a wait can
/// end its reader's blocked read ([`Jobserver::acquire`]). `SIGURG` found with a handler of
/// another's is given it back and refused, since taking it over would break its owner and its
/// owner's `SA_RESTART` would leave a stopped read blocked. A disposition of `SIG_IGN` (inherited
/// across `exec`) or `SIG_DFL` discards the signal, as the handler does in effect, so the handler
/// replaces it. Every later call answers as the first did.
/// # Errors
/// `sigaction` fails, or `SIGURG` already has another handler.
#[cfg(unix)]
fn interruptible() -> io::Result<()> {
    static ROUTED: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    ROUTED
        .get_or_init(|| {
            let ours = on_interrupt as extern "C" fn(libc::c_int) as libc::sighandler_t;
            // SAFETY: `sigaction` is plain data for which all-zero is a valid value; filled in
            // below, its flags left 0 (no `SA_RESTART`).
            let mut act: libc::sigaction = unsafe { std::mem::zeroed() };
            act.sa_sigaction = ours;
            // SAFETY: `act.sa_mask` is a valid out-parameter owned by this frame.
            unsafe { libc::sigemptyset(&raw mut act.sa_mask) };
            // SAFETY: as for `act`.
            let mut was: libc::sigaction = unsafe { std::mem::zeroed() };
            // SAFETY: installs `on_interrupt`, which does nothing and so is async-signal-safe;
            // `was` receives the disposition it replaces.
            if unsafe { libc::sigaction(INTERRUPT, &raw const act, &raw mut was) } != 0 {
                return Err(format!(
                    "route SIGURG to the jobserver's wait: {}",
                    io::Error::last_os_error()
                ));
            }
            if ![libc::SIG_DFL, libc::SIG_IGN, ours].contains(&was.sa_sigaction) {
                // SAFETY: restores the disposition just read into `was`.
                unsafe { libc::sigaction(INTERRUPT, &raw const was, std::ptr::null_mut()) };
                return Err(
                    "SIGURG already has a handler in this process: a jobserver wait could not \
                     end its read on a stop"
                        .to_owned(),
                );
            }
            Ok(())
        })
        .clone()
        .map_err(io::Error::other)
}

impl Drop for Jobserver {
    fn drop(&mut self) {
        if self.created {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// One implicit job. Its lifetime prevents removing the pool while a branch uses it. Dropped, it
/// goes to its handle's oldest wait for a token, or with none back to the FIFO
/// ([`Jobserver::give`]).
#[derive(Debug)]
pub struct Token<'a> {
    pool: &'a Jobserver,
    byte: u8,
}

impl Drop for Token<'_> {
    fn drop(&mut self) {
        self.pool.give(self.byte);
    }
}

#[cfg(all(test, unix))]
#[path = "jobserver_tests.rs"]
mod tests;
