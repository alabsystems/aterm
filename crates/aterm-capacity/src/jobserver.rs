// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! GNU FIFO jobservers (`docs/DESIGN-fast-build-release-2026-11-05.md`, A11 and B4).
//! Qualify partitions its Release grant with [`crate::BranchShares`], so an exiting branch's
//! lost tokens cannot starve another branch. Within each FIFO, clients reserve their implicit
//! token before spawning; Cargo and rustc borrow every additional token from that FIFO.
//!
//! THE POOL AND ITS CLIENTS. A coordinator makes a pool ([`Jobserver::new`]) and hands it to
//! its child ([`Jobserver::hand_to`], or a
//! share of fewer slots with [`Jobserver::hand_share_to`]: [`POOL_ENV`] is `<slots>:<absolute FIFO
//! path>`, the FIFO always by the pool's own path). A branch adopts it ([`Jobserver::inherited`])
//! and charges every client it starts from it: a Cargo through [`Jobserver::attach_cargo`] (its
//! implicit token, and the GNU auth its rustc workers draw the rest through), any other process
//! through [`Jobserver::acquire_many`] (its whole charge). So the jobs running across every branch
//! never hold more than `S` tokens, and only the pool's creator removes the FIFO.
//!
//! COUNTING. The GNU protocol names no holder, so a client that dies holding a token takes it
//! with it. Every FIFO [`Jobserver::new`] makes carries two custody files beside it, by which
//! the pool's one adopter, when none of its children is alive, counts the FIFO's bytes and the
//! tokens it holds itself against the allocation published beside the FIFO and makes the
//! difference good ([`Jobserver::count_alone`]): `<fifo>.transfer`, which the creator holds
//! while it moves an allocation ([`Jobserver::moving`]), and `<fifo>.clients`, which every
//! adopting handle holds shared for its life. Only a FIFO whose allocation is published beside
//! it (`<fifo>.slots`, as [`crate::BranchShares`] publishes each branch's) is counted.

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
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

/// The variable a release hands its branch children its pool in: `<slots>:<absolute FIFO path>`,
/// the slots a decimal count in `1..=4096` and the path free of whitespace, as
/// [`Jobserver::new`] requires ([`Jobserver::hand_to`], [`Jobserver::inherited`]).
pub const POOL_ENV: &str = "ATERM_RELEASE_POOL";

/// A pool delegated to a worker child while its caller retains one charged implicit job.
/// Same value format as [`POOL_ENV`]. One worker inherits that job; auxiliaries borrow tokens.
/// This advertisement creates no credit: the caller keeps its token through the child wait.
pub const IMPLICIT_POOL_ENV: &str = "ATERM_IMPLICIT_POOL";

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
    observed_slots: AtomicU32,
    /// The release grant an isolated branch can eventually receive. The coordinator publishes
    /// it beside the FIFO; unlike `observed_slots`, this is a Cargo dispatch ceiling, not the
    /// number of tokens currently assigned to the branch.
    observed_grant: AtomicU32,
    /// Whether this handle made the FIFO ([`Self::new`]), and so removes it when dropped; a
    /// handle adopted from [`POOL_ENV`] leaves it to its creator.
    created: bool,
    /// The custody files beside the FIFO ([`Custody`]); none where the creator made none.
    #[cfg(unix)]
    custody: Option<Custody>,
    /// How many [`Moving`] guards of this creator live: the first takes `<fifo>.transfer`, the
    /// last lets it go, so a move nested in another never lets go of the outer one's hold.
    moves: Mutex<u32>,
    /// Held shared across each FIFO read or write of a token this handle holds, together with
    /// the change to `out` it makes; a count holds it exclusive ([`Self::count_alone`]), so no
    /// token of this handle is between the FIFO and `out` while it counts.
    traffic: RwLock<()>,
    /// The tokens this handle has read from the FIFO and not yet written back: those its
    /// [`Token`]s hold and those its waits hold for their callers.
    out: AtomicU32,
    /// What a count reports ([`Self::note_reaped`], [`Self::count_alone`]).
    notes: Mutex<Notes>,
    /// Held through each count of this handle, taken without waiting ([`Self::count_alone`]).
    /// A count's hold of `<fifo>.transfer` is this handle's descriptor's, and `flock` belongs
    /// to the open file description: a second count of the handle would be granted the same
    /// lock, and the first count's end would let it go under the second, which the creator
    /// could then move an allocation beside.
    #[cfg(unix)]
    one_count: Mutex<()>,
    /// This handle's waits for a token now ([`Self::acquire`]).
    #[cfg(unix)]
    waiters: Mutex<Waiters>,
    /// Told when a wait's read ends or a token is handed to a wait.
    #[cfg(unix)]
    turn: Condvar,
}

/// The custody files beside a FIFO ([`Jobserver::count_alone`]).
#[cfg(unix)]
#[derive(Debug)]
struct Custody {
    /// `<fifo>.transfer`. The creator holds it exclusive while it moves an allocation into or
    /// out of the FIFO and while it holds tokens of the FIFO outside it ([`Jobserver::moving`]);
    /// a count holds it exclusive too, one count of a handle at a time (`one_count`: a second
    /// on the same descriptor would be granted the same lock), so no count sees half a transfer.
    transfer: File,
    /// `<fifo>.clients`, on an adopted handle: held shared for the handle's whole life, so a
    /// handle takes it exclusive only while no other handle has adopted the pool.
    clients: Option<File>,
}

/// The clients a caller reaped since this handle's last whole count, and which records it made.
#[derive(Debug, Default)]
struct Notes {
    /// The latest [`NOTED`] of them, oldest first: a note past that many drops the oldest.
    reaped: Vec<String>,
    /// How many earlier ones `reaped` no longer lists.
    unlisted: usize,
    /// Whether a count of this handle found a shortfall it made good.
    short_recorded: bool,
    /// Whether a count of this handle recorded a shortfall it could not judge.
    unjudged_recorded: bool,
    /// The latest record this handle wrote, the one its `capacity-event:` line was written from
    /// ([`Jobserver::count_event`]).
    last: Option<CountRecord>,
}

/// How many reaped clients a count's record lists: the latest, with how many earlier ones it
/// dropped.
const NOTED: usize = 64;

/// One count's record ([`Jobserver::count_event`]), as its `capacity-event:` line says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CountRecord {
    /// `short`, `over` or `short-unjudged`.
    pub(crate) outcome: &'static str,
    /// Whether no earlier record of this handle named the same kind of shortfall.
    pub(crate) first: bool,
    pub(crate) allocated: u32,
    pub(crate) in_fifo: u32,
    pub(crate) held: u32,
    /// The tokens written for a shortfall, or read out for a surplus.
    pub(crate) changed: u32,
    pub(crate) evidence: String,
    /// The clients reaped since the last whole count, and how many earlier ones were dropped.
    pub(crate) reaped: Vec<String>,
    pub(crate) unlisted: usize,
}

/// What [`Jobserver::count_alone`] found: the FIFO's bytes and the tokens the counting handle
/// holds, against the allocation published beside the FIFO.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenCount {
    /// Not judged, for the reason given; nothing was written or read.
    Unsettled(&'static str),
    /// The FIFO and this handle hold the whole allocation.
    Whole {
        /// The allocation published beside the FIFO.
        allocated: u32,
    },
    /// They held fewer: `reminted` new tokens made the FIFO whole.
    Short {
        /// The allocation published beside the FIFO.
        allocated: u32,
        /// The FIFO's bytes, this handle's tokens, and one for each of its waits in its read
        /// (IN-DOUBT READERS at [`Jobserver::count_alone`]).
        found: u32,
        /// The tokens written to make it whole.
        reminted: u32,
    },
    /// They held more: `discarded` surplus bytes were read from the FIFO and dropped.
    Over {
        /// The allocation published beside the FIFO.
        allocated: u32,
        /// The FIFO's bytes and this handle's tokens.
        found: u32,
        /// The bytes taken out of circulation, at most the FIFO's.
        discarded: u32,
    },
}

impl TokenCount {
    /// The tokens this count wrote into the FIFO.
    #[must_use]
    pub fn reminted(&self) -> u32 {
        match self {
            Self::Short { reminted, .. } => *reminted,
            _ => 0,
        }
    }
}

/// The creator's exclusive hold of a FIFO's `<fifo>.transfer` ([`Jobserver::moving`]): the pool
/// holds the lock while any of its guards lives, and the last to drop lets it go by `LOCK_UN`
/// (clippy.toml's flock release rule). Every re-mint and every take of a token to move it out
/// asks for one ([`Jobserver::remint`], [`Jobserver::try_acquire_moving`]), so no move can be
/// written outside the lock.
#[derive(Debug)]
#[must_use = "the transfer lock is held only while this guard lives"]
pub(crate) struct Moving<'a> {
    pool: &'a Jobserver,
    /// Whether this guard counts in the pool's `moves`: a pool with no custody files, an
    /// adopted one, or a lock the kernel refused holds nothing.
    held: bool,
}

impl Drop for Moving<'_> {
    fn drop(&mut self) {
        if !self.held {
            return;
        }
        let mut moves = self
            .pool
            .moves
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *moves -= 1;
        #[cfg(unix)]
        if *moves == 0
            && let Some(custody) = &self.pool.custody
        {
            let _ = custody.transfer.unlock();
        }
    }
}

/// A count's own exclusive hold of an adopter's `<fifo>.transfer` ([`Jobserver::count_alone`]),
/// let go by `LOCK_UN` as it drops.
#[cfg(unix)]
struct Counting<'a>(&'a File);

#[cfg(unix)]
impl Drop for Counting<'_> {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
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
    /// Create a new pool in a private release directory: the FIFO at `path`, which must not
    /// exist yet, and its custody files beside it (`<fifo>.transfer`, `<fifo>.clients`, by
    /// [`Path::with_extension`]), which replace any a dead run left there.
    /// # Errors
    /// The token count or FIFO path cannot be represented, or creating/opening it fails.
    pub fn new(path: &Path, slots: u32) -> io::Result<Self> {
        Self::seeded(path, slots, slots)
    }

    /// Create a FIFO with a bounded initial allocation, for an isolated branch pool.
    pub(crate) fn seeded(path: &Path, slots: u32, tokens: u32) -> io::Result<Self> {
        representable(path, slots)?;
        if tokens > slots {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "allocation exceeds slots",
            ));
        }
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
                file.write_all(&[b'+'; MAX_SLOTS as usize][..tokens as usize])?;
                // The FIFO is new, so files beside it are a dead run's: an adopter of that run's
                // FIFO holding them is no client of this one, and must not hold off its counts.
                for kind in CUSTODY {
                    let _ = std::fs::remove_file(path.with_extension(kind));
                }
                let transfer = custody_file(&path.with_extension(CUSTODY[0]), true)?;
                custody_file(&path.with_extension(CUSTODY[1]), true)?;
                Ok((
                    file,
                    wait,
                    Custody {
                        transfer,
                        clients: None,
                    },
                ))
            });
            let (file, wait, custody) = match opened {
                Ok(all) => all,
                Err(error) => {
                    let _ = std::fs::remove_file(&path);
                    for kind in CUSTODY {
                        let _ = std::fs::remove_file(path.with_extension(kind));
                    }
                    return Err(error);
                }
            };
            Ok(Self::holding(path, file, wait, slots, true, Some(custody)))
        }
        #[cfg(not(unix))]
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "FIFO jobservers require Unix",
        ))
    }

    /// The pool a release handed this process ([`POOL_ENV`], [`Self::hand_to`]): `None` when the
    /// variable is unset, and an error when it is set but malformed, names no FIFO this user
    /// owns, or names one whose custody files cannot be opened or locked ([`Self::adopt`]).
    #[must_use]
    pub fn inherited() -> Option<io::Result<Self>> {
        std::env::var_os(POOL_ENV).map(|value| Self::adopt(&value))
    }

    /// Adopt the pool `value` names, in [`POOL_ENV`]'s form `<slots>:<absolute FIFO path>`. The
    /// FIFO is opened as [`Self::new`] opens it, and is never removed by this handle: the pool
    /// is its creator's. No token is taken or added.
    /// # Errors
    /// `value` is not of that form, the path does not open as a FIFO this user owns, or a
    /// custody file beside it (`<fifo>.transfer`, `<fifo>.clients`) exists but cannot be opened
    /// (a link among them), or `<fifo>.clients` cannot be held shared.
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
            // A pool whose creator made no custody files is never counted.
            let absent = |opened: io::Result<File>| match opened {
                Ok(file) => Ok(Some(file)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error),
            };
            let transfer = absent(custody_file(&path.with_extension(CUSTODY[0]), false))?;
            let clients = absent(custody_file(&path.with_extension(CUSTODY[1]), false))?;
            let custody = match (transfer, clients) {
                (Some(transfer), Some(clients)) => {
                    // Held for the handle's life, before it can take a token.
                    locked(&clients, false)?;
                    Some(Custody {
                        transfer,
                        clients: Some(clients),
                    })
                }
                _ => None,
            };
            Ok(Self::holding(
                path.to_owned(),
                file,
                wait,
                slots,
                false,
                custody,
            ))
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
    fn holding(
        path: PathBuf,
        file: File,
        wait: File,
        slots: u32,
        created: bool,
        custody: Option<Custody>,
    ) -> Self {
        Self {
            path,
            file,
            wait,
            slots,
            observed_slots: AtomicU32::new(slots),
            observed_grant: AtomicU32::new(slots),
            created,
            custody,
            moves: Mutex::new(0),
            traffic: RwLock::new(()),
            out: AtomicU32::new(0),
            notes: Mutex::default(),
            one_count: Mutex::new(()),
            waiters: Mutex::default(),
            turn: Condvar::new(),
        }
    }

    /// The dispatch budget retained by this handle. A plain or creating handle reports its
    /// whole pool; an adopted branch keeps its largest published allocation. Lending a free
    /// token does not revoke a client's budget: the FIFO still bounds live work. Use
    /// [`Self::grant`] for Cargo's eventual dispatch ceiling.
    #[must_use]
    pub fn slots(&self) -> u32 {
        if self.created {
            return self.slots;
        }
        // A client keeps its largest observed dispatch budget. The coordinator can lend a
        // free token to another branch without revoking in-flight work; the FIFO remains the
        // concurrency bound. Published growth lets later test plans use returned shares.
        let observed = self
            .published_share()
            .map(|(grant, allocated)| {
                self.observed_grant.fetch_max(grant, Ordering::Relaxed);
                allocated.max(self.slots)
            })
            .unwrap_or(self.slots);
        self.observed_slots.fetch_max(observed, Ordering::Relaxed);
        self.observed_slots.load(Ordering::Relaxed)
    }

    /// The release grant this FIFO can eventually receive. An isolated branch starts with only
    /// [`Self::slots`] tokens, but Cargo must retain this wider dispatch ceiling so the same live
    /// client can consume tokens reminted when another branch ends. The FIFO, not this ceiling,
    /// remains the concurrency bound. A plain pool has no side record and returns its own slots.
    #[must_use]
    pub fn grant(&self) -> u32 {
        if self.created {
            return self.slots;
        }
        if let Some((grant, _)) = self.published_share() {
            self.observed_grant.fetch_max(grant, Ordering::Relaxed);
        }
        self.observed_grant.load(Ordering::Relaxed)
    }

    fn published_share(&self) -> Option<(u32, u32)> {
        let value = std::fs::read_to_string(self.path.with_extension("slots")).ok()?;
        let (grant, allocated) = value.split_once(':')?;
        let grant = grant.parse::<u32>().ok()?;
        let allocated = allocated.parse::<u32>().ok()?;
        (grant <= MAX_SLOTS && allocated <= grant && grant >= self.slots)
            .then_some((grant, allocated))
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
        self.value(self.slots())
    }

    /// [`POOL_ENV`]'s value for a share of this pool, `slots` of its tokens: [`Self::handed`]'s
    /// form with that count and this pool's own FIFO path ([`Self::path`]), so a child handed a
    /// share and one handed the whole pool are handed one spelling of one FIFO. A child that
    /// adopts it reads the share as its pool's slots and sizes its charges to them.
    /// # Errors
    /// `slots` is 0, which [`Self::adopt`] refuses, or more than [`Self::slots`], which would let
    /// the child wait for tokens the pool does not have.
    pub fn handed_share(&self, slots: u32) -> io::Result<OsString> {
        if slots == 0 || slots > self.slots() {
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
            .arg(self.grant().to_string());
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
        self.acquire_notifying(stopped, Duration::from_secs(30), &|elapsed| {
            let allocation = std::fs::read_to_string(self.path.with_extension("allocation"))
                .unwrap_or_else(|error| format!("allocation_record_unavailable={error}"));
            eprintln!("jobserver empty wait: waited_s={} waiter_pid={} dispatch_slots={} fifo={} {allocation}",
                elapsed.as_secs(), std::process::id(), self.slots, self.path.display());
        })
    }

    fn acquire_notifying(
        &self,
        stopped: &dyn Fn() -> bool,
        after: Duration,
        notify: &dyn Fn(Duration),
    ) -> io::Result<Token<'_>> {
        let started = std::time::Instant::now();
        let asked_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|d| u64::try_from(d.as_millis()).ok());
        let stated = std::cell::Cell::new(false);
        let waited = std::cell::Cell::new(false);
        let result = self.acquire_waking(stopped, WAKE_EVERY, &|| {
            waited.set(true);
            if started.elapsed() >= after && !stated.replace(true) {
                notify(started.elapsed());
            }
        });
        if waited.get() {
            let ended_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|d| u64::try_from(d.as_millis()).ok());
            eprintln!(
                "capacity-event: {}",
                aterm_json::json!({
                    "schema":1,"event":"jobserver-wait","process_id":std::process::id(),
                    "fifo":self.path,"slots":self.slots,"resource":"core",
                    "started_at_unix_ms":asked_ms,"ended_at_unix_ms":ended_ms,
                    "waiting_s":started.elapsed().as_secs_f64(),"acquired":result.is_ok(),
                    "bounds":"acquire call; the FIFO was observed empty",
                })
            );
        }
        result
    }

    /// Re-mint an allocation whose former branch has drained. Only the share accountant calls
    /// this, under this pool's transfer lock (`moving`); returning a token from a live client
    /// must use its guard instead.
    /// # Panics
    /// `moving` is another pool's guard.
    pub(crate) fn remint(&self, tokens: u32, moving: &Moving<'_>) {
        assert!(
            std::ptr::eq(moving.pool, self),
            "a re-mint holds its own pool's transfer lock"
        );
        for _ in 0..tokens {
            self.give(b'+');
        }
    }

    /// [`Self::try_acquire`] for a token the share accountant takes out of this FIFO to move or
    /// drain it, under this pool's transfer lock (`moving`).
    /// # Errors
    /// As [`Self::try_acquire`].
    /// # Panics
    /// `moving` is another pool's guard.
    pub(crate) fn try_acquire_moving(&self, moving: &Moving<'_>) -> io::Result<Option<Token<'_>>> {
        assert!(
            std::ptr::eq(moving.pool, self),
            "a move's take holds its own pool's transfer lock"
        );
        self.try_acquire()
    }

    /// Hold this pool's `<fifo>.transfer` exclusive, waiting for a count that holds it: the
    /// creator does so while it moves an allocation into or out of the FIFO, and while it holds
    /// tokens of the FIFO outside it, from the first byte it reads or the allocation it
    /// publishes to the last byte it writes, so no adopter's count sees half of either. Guards
    /// nest: the pool holds the lock while any lives. A handle that did not create the FIFO, or
    /// one that made no custody files, holds nothing. A creator that holds tokens of its own
    /// outside this lock while an adopter lives breaks [`Self::count_alone`]'s premise. LIMIT: a
    /// lock the kernel refuses (never seen for this local regular file: `flock` fails on it
    /// only on `EINTR`, which is retried) is said on stderr and not held.
    pub(crate) fn moving(&self) -> Moving<'_> {
        #[cfg(unix)]
        if let Some(custody) = self.custody.as_ref().filter(|_| self.created) {
            let mut moves = self.moves.lock().unwrap_or_else(PoisonError::into_inner);
            // Taken by name here, not through `locked`, so the lock-order census sees this
            // file's own lock under `moves`: no count that holds it ever waits for `moves`.
            let transfer = &custody.transfer;
            if *moves == 0 {
                loop {
                    #[expect(
                        clippy::disallowed_methods,
                        reason = "held for the pool's guards: the last `Moving` releases by LOCK_UN"
                    )]
                    let taken = transfer.lock();
                    match taken {
                        Ok(()) => break,
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(error) => {
                            eprintln!(
                                "jobserver transfer lock of {} unavailable: {error}",
                                self.path.display()
                            );
                            return Moving {
                                pool: self,
                                held: false,
                            };
                        }
                    }
                }
            }
            *moves += 1;
            return Moving {
                pool: self,
                held: true,
            };
        }
        Moving {
            pool: self,
            held: false,
        }
    }

    /// Note a client of this pool that its caller has reaped, for the record of the next count
    /// that is not whole ([`Self::count_alone`], [`crate::BranchShares::reaped`]). A whole count
    /// forgets the clients noted before it, and only the latest [`NOTED`] are listed: an earlier
    /// one is dropped and only counted.
    pub fn note_reaped(&self, client: String) {
        let mut notes = self.notes.lock().unwrap_or_else(PoisonError::into_inner);
        if notes.reaped.len() == NOTED {
            notes.reaped.remove(0);
            notes.unlisted += 1;
        }
        notes.reaped.push(client);
    }

    /// The clients noted since the last whole count, and how many more were not listed; the
    /// notes are cleared.
    pub(crate) fn take_reaped(&self) -> (Vec<String>, usize) {
        let mut notes = self.notes.lock().unwrap_or_else(PoisonError::into_inner);
        (
            std::mem::take(&mut notes.reaped),
            std::mem::take(&mut notes.unlisted),
        )
    }

    /// Whether no handle has adopted this pool now: `<fifo>.clients` taken exclusive and let go
    /// at once. `None` where the pool has no custody files or the lock cannot be asked.
    pub(crate) fn adopters_gone(&self) -> Option<bool> {
        #[cfg(unix)]
        {
            let clients = custody_file(&self.path.with_extension(CUSTODY[1]), false).ok()?;
            #[expect(
                clippy::disallowed_methods,
                reason = "an instant probe: let go by LOCK_UN on the next statement"
            )]
            let probed = clients.try_lock();
            match probed {
                Ok(()) => {
                    let _ = clients.unlock();
                    Some(true)
                }
                Err(std::fs::TryLockError::WouldBlock) => Some(false),
                Err(std::fs::TryLockError::Error(_)) => None,
            }
        }
        #[cfg(not(unix))]
        None
    }

    /// Count this pool's tokens as its one adopter, with none of its clients alive: the bytes
    /// in the FIFO and the tokens this handle holds (its [`Token`]s and its waits' tokens)
    /// against the allocation the creator published beside the FIFO (`<fifo>.slots`). A
    /// shortfall is a token a client took with it when it died, so it is written back, and a
    /// surplus, a token minted twice, is read out; either writes a `capacity-event:` record
    /// with the clients reaped since the last whole count ([`Self::note_reaped`]).
    ///
    /// IN-DOUBT READERS. A wait of this handle whose reader is in its read (`Reading`) may or
    /// may not have taken a byte it has not yet counted as held, so the tokens there are lie
    /// between the FIFO's bytes and this handle's (`lo`) and `lo` plus one per such reader
    /// (`hi`). The count is short only when `hi` is below the allocation, and writes back no more
    /// than that gap; it is over only when `lo` is above it, and reads out no more than that
    /// excess; otherwise, with a reader in doubt, it is unjudged. So no count mints a byte a
    /// reader took, and none reads out a byte a reader has yet to take.
    ///
    /// SOUND ONLY ON EVIDENCE. Nothing but the following can hold a token of the FIFO, and the
    /// count is judged only while each is excluded; otherwise it answers
    /// [`TokenCount::Unsettled`] and changes nothing:
    /// - the creator, which holds `<fifo>.transfer` while it moves an allocation or holds
    ///   tokens outside the FIFO ([`Self::moving`]): the count takes it exclusive, without
    ///   waiting;
    /// - another adopting handle, in this process or another, which holds `<fifo>.clients`
    ///   shared while it lives: the count takes it exclusive, without waiting;
    /// - another thread of this handle, mid-way between the FIFO and its count of what it
    ///   holds: the count holds that traffic off, without waiting;
    /// - another count of this handle (ONE COUNT AT A TIME: both would hold `<fifo>.transfer`
    ///   by the same descriptor, so the lock keeps neither from the other, and the first's end
    ///   would let it go while the second counts): the count takes the handle's own count
    ///   lock, without waiting;
    /// - a child of the caller's, or its descendant, which a GNU client among them may draw
    ///   from: `alone`, asked once every other exclusion is held, before the FIFO's size is
    ///   read and again after it, must say both times that none is alive. A child alive while
    ///   the size is read is alive at one of the two answers, unless the caller both started
    ///   and reaped it between them; this handle's own tokens cannot move until the count ends.
    ///
    /// A shortfall judged on weaker evidence (another adopter, or a child alive) is recorded
    /// once per handle as unjudged, with nothing written. LIMIT: a process `alone` cannot see
    /// holding a token (a descendant that left its process group, a member of a group whose
    /// leader the caller reaped while it lives on, or a child started and reaped between the
    /// two answers that returned a token after the size was read) is counted as lost and
    /// written back, so until a later count reads out the surplus its return makes, the pool
    /// holds that many tokens more than its allocation.
    /// # Errors
    /// The custody lock, the FIFO's size or its write or read fails; whatever was written or
    /// read before the error stays.
    pub fn count_alone(&self, alone: &dyn Fn() -> bool) -> io::Result<TokenCount> {
        #[cfg(unix)]
        {
            let Some(Custody {
                transfer,
                clients: Some(clients),
            }) = &self.custody
            else {
                return Ok(TokenCount::Unsettled(
                    "only an adopted FIFO whose creator keeps custody files is counted",
                ));
            };
            // Held to the count's end, so it lets go after the transfer lock (ONE COUNT AT A
            // TIME).
            let _one = match self.one_count.try_lock() {
                Ok(one) => one,
                Err(std::sync::TryLockError::Poisoned(one)) => one.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    return Ok(TokenCount::Unsettled(
                        "another count of this handle is running",
                    ));
                }
            };
            #[expect(
                clippy::disallowed_methods,
                reason = "taken into the `Counting` guard below, whose Drop releases by LOCK_UN"
            )]
            let free = transfer.try_lock();
            match free {
                Ok(()) => {}
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Ok(TokenCount::Unsettled(
                        "the creator is moving an allocation or holds tokens outside the FIFO",
                    ));
                }
                Err(std::fs::TryLockError::Error(error)) => return Err(error),
            }
            let counting = Counting(transfer);
            // Held through the count: no token of this handle is given while it counts, and a
            // wait's reader moves between `Reading` and holding its byte only under it.
            let waiters = self.waiters.lock().unwrap_or_else(PoisonError::into_inner);
            let Ok(traffic) = self.traffic.try_write() else {
                return Ok(TokenCount::Unsettled("a token of this handle is moving"));
            };
            // A reader in its read may or may not have taken a byte it has not yet counted as
            // held (IN-DOUBT READERS).
            let reading = waiters
                .each
                .values()
                .filter(|w| matches!(w.reader, Reader::Reading(_)))
                .count();
            let reading = u32::try_from(reading).unwrap_or(u32::MAX);
            // Shared to exclusive: no other counter runs (this one holds the transfer lock and
            // its handle's count lock), so the moment between the two holds lets no one in who
            // could count.
            let _ = clients.unlock();
            #[expect(
                clippy::disallowed_methods,
                reason = "returned to a shared hold by the `Shared` guard below"
            )]
            let sole = clients.try_lock();
            let shared = Shared(clients);
            let unjudged = match sole {
                Ok(()) => (!alone()).then_some("a child of this process may hold a token"),
                Err(std::fs::TryLockError::WouldBlock) => {
                    Some("another handle has adopted this pool")
                }
                Err(std::fs::TryLockError::Error(error)) => return Err(error),
            };
            let Some((_, allocated)) = self.published_share() else {
                return Ok(TokenCount::Unsettled(
                    "no allocation is published beside the FIFO",
                ));
            };
            let in_fifo = available(&self.path, &self.file)?;
            // Asked again: a child started after the first answer that took a byte before the
            // size was read is still alive now, or died holding it.
            let unjudged = unjudged
                .or_else(|| (!alone()).then_some("a child of this process may hold a token"));
            let lo = in_fifo.saturating_add(self.out.load(Ordering::SeqCst));
            let hi = lo.saturating_add(reading);
            let held = hi - in_fifo;
            if let Some(why) = unjudged {
                if hi < allocated {
                    self.count_event("short-unjudged", allocated, in_fifo, held, 0, why);
                }
                return Ok(TokenCount::Unsettled(why));
            }
            let count = if hi < allocated {
                let reminted = allocated - hi;
                (&self.file).write_all(&[b'+'; MAX_SLOTS as usize][..reminted as usize])?;
                TokenCount::Short {
                    allocated,
                    found: hi,
                    reminted,
                }
            } else if lo > allocated {
                let mut surplus = vec![0; (lo - allocated).min(in_fifo) as usize];
                let discarded = match (&self.file).read(&mut surplus) {
                    Ok(read) => read,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => 0,
                    Err(error) => return Err(error),
                };
                TokenCount::Over {
                    allocated,
                    found: lo,
                    discarded: u32::try_from(discarded).unwrap_or(u32::MAX),
                }
            } else if reading > 0 {
                return Ok(TokenCount::Unsettled(
                    "a wait of this handle is in its read",
                ));
            } else {
                TokenCount::Whole { allocated }
            };
            drop((shared, traffic, waiters, counting));
            match count {
                TokenCount::Whole { .. } | TokenCount::Unsettled(_) => drop(self.take_reaped()),
                TokenCount::Short { reminted, .. } => {
                    self.count_event("short", allocated, in_fifo, held, reminted, "sole client");
                }
                TokenCount::Over { discarded, .. } => {
                    self.count_event("over", allocated, in_fifo, held, discarded, "sole client");
                }
            }
            Ok(count)
        }
        #[cfg(not(unix))]
        {
            let _ = alone;
            Ok(TokenCount::Unsettled("FIFO jobservers require Unix"))
        }
    }

    /// Write one count's `capacity-event:` record and return its clients: `outcome` (`short`,
    /// `over` or `short-unjudged`), the published `allocated`, the FIFO's bytes and the tokens
    /// the counter held, the tokens `changed` (written for a shortfall, read out for a surplus)
    /// and the `evidence` it was judged on, with the clients reaped since the last whole count
    /// (the latest [`NOTED`], and how many earlier ones were dropped). A judged count clears
    /// them; an unjudged shortfall leaves them for the next judged count. `first` says whether
    /// no earlier record of this handle named the same kind of shortfall; an unjudged one after
    /// the first is not written again, and returns no clients. The record is kept as the
    /// handle's latest (`Notes::last`).
    pub(crate) fn count_event(
        &self,
        outcome: &'static str,
        allocated: u32,
        in_fifo: u32,
        held: u32,
        changed: u32,
        evidence: &str,
    ) -> Vec<String> {
        // Only the notes' own fields are read and written under their lock.
        let (first, reaped, unlisted) = {
            let mut notes = self.notes.lock().unwrap_or_else(PoisonError::into_inner);
            let first = match outcome {
                "short" => !std::mem::replace(&mut notes.short_recorded, true),
                "short-unjudged" => !std::mem::replace(&mut notes.unjudged_recorded, true),
                _ => false,
            };
            if outcome != "short-unjudged" {
                (
                    first,
                    Some(std::mem::take(&mut notes.reaped)),
                    std::mem::take(&mut notes.unlisted),
                )
            } else if first {
                (first, Some(notes.reaped.clone()), notes.unlisted)
            } else {
                (first, None, 0)
            }
        };
        let Some(reaped) = reaped else {
            return Vec::default();
        };
        let record = CountRecord {
            outcome,
            first,
            allocated,
            in_fifo,
            held,
            changed,
            evidence: evidence.to_owned(),
            reaped,
            unlisted,
        };
        eprintln!(
            "capacity-event: {}",
            aterm_json::json!({
                "schema":1,"event":"jobserver-count","process_id":std::process::id(),
                "fifo":self.path,"outcome":record.outcome,"first":record.first,
                "allocated":record.allocated,"in_fifo":record.in_fifo,
                "held_by_counter":record.held,"changed":record.changed,
                "evidence":record.evidence,"reaped_since_whole_count":record.reaped,
                "reaped_unlisted":record.unlisted,
            })
        );
        let reaped = record.reaped.clone();
        self.notes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .last = Some(record);
        reaped
    }

    /// The latest record this handle wrote ([`Self::count_event`]).
    #[cfg(test)]
    pub(crate) fn last_count(&self) -> Option<CountRecord> {
        self.notes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .last
            .clone()
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
        // Counted as held under the lock that ends `Reading`, so a count, which holds it, finds
        // a byte this reader took either here or among the readers it cannot judge.
        if let Ok(Some(_)) = read {
            self.out.fetch_add(1, Ordering::SeqCst);
        }
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
            let _ = self.returned(&[byte]);
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
                let _ = self.returned(&[byte]);
                self.turn.notify_all();
            }
        }
    }

    /// Write `bytes` this handle held back to the FIFO, and stop counting them as held.
    /// # Errors
    /// The write fails: what it did not write is lost, and still counted as held.
    fn returned(&self, bytes: &[u8]) -> io::Result<()> {
        // Holds off a count while this token moves between the FIFO and `out`.
        let _passing = self.traffic.read().unwrap_or_else(PoisonError::into_inner);
        (&self.file).write_all(bytes)?;
        let n = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        let _ = self
            .out
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |out| {
                Some(out.saturating_sub(n))
            });
        Ok(())
    }

    /// Read into `bytes` from the FIFO without waiting, counting what is read as held.
    fn taken(&self, bytes: &mut [u8]) -> io::Result<usize> {
        // Holds off a count while this token moves between the FIFO and `out`.
        let _passing = self.traffic.read().unwrap_or_else(PoisonError::into_inner);
        let read = (&self.file).read(bytes)?;
        self.out
            .fetch_add(u32::try_from(read).unwrap_or(u32::MAX), Ordering::SeqCst);
        Ok(read)
    }

    /// Take `n` tokens, at most the pool's slots, for one process that is no GNU client (a test
    /// binary, whose threads and children run on its charge), returned when the guards drop.
    ///
    /// A whole charge is one nonblocking FIFO read. A short read is returned before waiting,
    /// so a large test cannot park idle tokens behind a live compiler's implicit job, and
    /// competing whole charges cannot deadlock by each retaining a fraction. Single-token
    /// requests keep [`Self::acquire`]'s fairness against Cargo's blocked readers.
    /// # Errors
    /// FIFO IO fails, or the caller ends the wait; the tokens taken so far are returned.
    pub fn acquire_many(&self, n: u32, stopped: &dyn Fn() -> bool) -> io::Result<Vec<Token<'_>>> {
        self.acquire_batch(n.min(self.slots()) as usize, stopped, &|| {})
    }

    fn acquire_batch(
        &self,
        n: usize,
        stopped: &dyn Fn() -> bool,
        waiting: &dyn Fn(),
    ) -> io::Result<Vec<Token<'_>>> {
        if n == 0 {
            return Ok(Vec::new());
        }
        if n == 1 {
            return self.acquire(stopped).map(|token| vec![token]);
        }
        let mut bytes = vec![0; n];
        let began = Instant::now();
        let mut stated = false;
        loop {
            if stopped() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "jobserver wait ended",
                ));
            }
            let count = match self.taken(&mut bytes) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "jobserver FIFO closed",
                    ));
                }
                Ok(count) => count,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    0
                }
                Err(error) => return Err(error),
            };
            let taken = if count == n {
                Some(
                    bytes
                        .iter()
                        .map(|&byte| Token { pool: self, byte })
                        .collect::<Vec<_>>(),
                )
            } else {
                // This restores availability rather than completing a job. Return directly
                // to the FIFO: notifying batch waiters here would let short reads wake each
                // other forever without any new capacity becoming free.
                if count != 0 {
                    self.returned(&bytes[..count])?;
                }
                None
            };
            // Short reads are back already; guards return a full grant on a stop or panic.
            if stopped() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "jobserver wait ended",
                ));
            }
            if let Some(taken) = taken {
                return Ok(taken);
            }
            waiting();
            if !stated && began.elapsed() >= Duration::from_secs(30) {
                stated = true;
                let allocation = std::fs::read_to_string(self.path.with_extension("allocation"))
                    .unwrap_or_else(|error| format!("allocation_record_unavailable={error}"));
                eprintln!(
                    "jobserver whole-charge wait: waited_s={} requested_tokens={n} waiter_pid={} dispatch_slots={} fifo={} {allocation}",
                    began.elapsed().as_secs(),
                    std::process::id(),
                    self.slots(),
                    self.path.display()
                );
            }
            // Local returns wake this wait. External GNU writers are observed at the stop
            // interval: readability alone cannot wait for a whole charge while a fraction sits
            // unread, and returning a short read must not become a busy reacquisition loop.
            #[cfg(unix)]
            {
                let waiters = self.waiters.lock().unwrap_or_else(PoisonError::into_inner);
                drop(
                    self.turn
                        .wait_timeout(waiters, WAKE_EVERY)
                        .unwrap_or_else(PoisonError::into_inner),
                );
            }
            #[cfg(not(unix))]
            std::thread::sleep(WAKE_EVERY);
        }
    }

    /// Take a token only if one is currently available.
    /// Interrupted reads are retried; only `WouldBlock` reports an empty FIFO.
    /// # Errors
    /// FIFO IO fails.
    pub fn try_acquire(&self) -> io::Result<Option<Token<'_>>> {
        // Holds off a count while this token moves between the FIFO and `out` ([`Self::taken`]).
        let _passing = self.traffic.read().unwrap_or_else(PoisonError::into_inner);
        let byte = read_token(&mut &self.file)?;
        if byte.is_some() {
            self.out.fetch_add(1, Ordering::SeqCst);
        }
        Ok(byte.map(|byte| Token { pool: self, byte }))
    }
}

fn read_token(input: &mut impl Read) -> io::Result<Option<u8>> {
    let mut byte = [0];
    loop {
        match input.read(&mut byte) {
            Ok(1) => return Ok(Some(byte[0])),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
            Err(error) => return Err(error),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "jobserver FIFO closed",
                ));
            }
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

/// The custody files' extensions beside a FIFO: `<fifo>.transfer`, then `<fifo>.clients`
/// ([`Custody`]).
#[cfg(unix)]
const CUSTODY: [&str; 2] = ["transfer", "clients"];

/// Open a custody file beside a FIFO, made new (0600) when `make`, out of a concurrent spawn's
/// descriptors and never through a link.
#[cfg(unix)]
fn custody_file(path: &Path, make: bool) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    aterm_uds::kept_out(|| {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(make)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)
    })
}

/// Take `file`'s lock, exclusive or shared, waiting for a holder of the other kind; a signal
/// that ends the wait is waited through.
/// # Errors
/// `flock` refuses.
#[cfg(unix)]
fn locked(file: &File, exclusive: bool) -> io::Result<()> {
    loop {
        #[expect(
            clippy::disallowed_methods,
            reason = "a custody hold: `Moving`, `Shared` and the handle's Drop release by LOCK_UN"
        )]
        let taken = if exclusive {
            file.lock()
        } else {
            file.lock_shared()
        };
        match taken {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            taken => return taken,
        }
    }
}

/// An adopter's `<fifo>.clients`, returned to its shared hold as this drops: after a count took
/// it exclusive, or tried to and found another adopter ([`Jobserver::count_alone`]).
#[cfg(unix)]
struct Shared<'a>(&'a File);

#[cfg(unix)]
impl Drop for Shared<'_> {
    fn drop(&mut self) {
        if let Err(error) = locked(self.0, false) {
            eprintln!("jobserver adopter's clients lock not restored: {error}");
        }
    }
}

/// The bytes waiting in the FIFO at `path`, which `file` holds, read without taking one
/// (`FIONREAD`). It asks a read-only descriptor of its own: Darwin answers `FIONREAD` on a FIFO
/// opened both ways from its write side, which holds nothing (0 for a FIFO of 4 tokens).
/// # Errors
/// The open or the ioctl fails, or `path` names another file by now.
#[cfg(unix)]
fn available(path: &Path, file: &File) -> io::Result<u32> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let reader = aterm_uds::kept_out(|| {
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(path)
    })?;
    let (ours, theirs) = (file.metadata()?, reader.metadata()?);
    if (ours.dev(), ours.ino()) != (theirs.dev(), theirs.ino()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is no longer the FIFO first opened", path.display()),
        ));
    }
    let mut count: libc::c_int = 0;
    // SAFETY: `reader` owns a live read-only FIFO descriptor; FIONREAD writes exactly one c_int
    // into `count` and consumes no byte.
    if unsafe { libc::ioctl(reader.as_raw_fd(), libc::FIONREAD, &raw mut count) } < 0 {
        return Err(io::Error::last_os_error());
    }
    u32::try_from(count).map_err(|_| io::Error::other("negative FIFO byte count"))
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
        #[cfg(unix)]
        if let Some(clients) = self.custody.as_ref().and_then(|c| c.clients.as_ref()) {
            let _ = clients.unlock();
        }
        if self.created {
            let _ = std::fs::remove_file(&self.path);
            #[cfg(unix)]
            for kind in CUSTODY {
                let _ = std::fs::remove_file(self.path.with_extension(kind));
            }
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
