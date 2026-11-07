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

use std::ffi::{OsStr, OsString};
use std::fs::File;
#[cfg(unix)]
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

/// The variable a release hands its branch children its pool in: `<slots>:<absolute FIFO path>`,
/// the slots a decimal count in `1..=4096` and the path free of whitespace, as
/// [`Jobserver::new`] requires ([`Jobserver::hand_to`], [`Jobserver::inherited`]).
pub const POOL_ENV: &str = "ATERM_RELEASE_POOL";

/// The most tokens a pool holds.
const MAX_SLOTS: u32 = 4096;

/// How long a wait for a token sleeps in `poll(2)` before it asks its stop predicate again
/// ([`Jobserver::acquire`]). It never delays a token: a write into the FIFO wakes the wait.
const WAKE_EVERY: Duration = Duration::from_millis(100);

/// A release-local pool, kept alive until all branch guards have been returned.
#[derive(Debug)]
pub struct Jobserver {
    path: PathBuf,
    file: File,
    slots: u32,
    /// Whether this handle made the FIFO ([`Self::new`]), and so removes it when dropped; a
    /// handle adopted from [`POOL_ENV`] leaves it to its creator.
    created: bool,
    /// Held across [`Self::acquire_many`]: this handle's one partial holder.
    partial: Mutex<()>,
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
            let mut file = match open(&path) {
                Ok(file) => file,
                Err(error) => {
                    let _ = std::fs::remove_file(&path);
                    return Err(error);
                }
            };
            if let Err(error) = file.write_all(&[b'+'; MAX_SLOTS as usize][..slots as usize]) {
                let _ = std::fs::remove_file(&path);
                return Err(error);
            }
            Ok(Self {
                path,
                file,
                slots,
                created: true,
                partial: Mutex::new(()),
            })
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
            Ok(Self {
                path: path.to_owned(),
                file,
                slots,
                created: false,
                partial: Mutex::new(()),
            })
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
    /// FAIR AGAINST BLOCKED READERS. Cargo's jobserver helper waits in a blocking `read(2)` on
    /// its own descriptor of the FIFO, and the kernel hands each token written back to a reader
    /// it wakes. This wait sleeps in `poll(2)` on the FIFO, so the same write wakes it beside
    /// them and it contends for that byte as one of them. A loop that slept between nonblocking
    /// reads would only ever see a byte in the instant between its write and a blocked reader
    /// taking it, and so lose nearly every one to a busy build. The poll's bound
    /// ([`WAKE_EVERY`]) only sets how soon `stopped` is asked again.
    /// # Errors
    /// FIFO IO fails, or the caller ends the wait.
    pub fn acquire(&self, stopped: &dyn Fn() -> bool) -> io::Result<Token<'_>> {
        self.acquire_waking(stopped, WAKE_EVERY, &|| {})
    }

    /// [`Self::acquire`], asking `stopped` at least once every `wake`, and telling `waiting` each
    /// time it found the FIFO empty and is about to wait (a test's hook).
    fn acquire_waking(
        &self,
        stopped: &dyn Fn() -> bool,
        wake: Duration,
        waiting: &dyn Fn(),
    ) -> io::Result<Token<'_>> {
        loop {
            if stopped() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "jobserver wait ended",
                ));
            }
            if let Some(token) = self.try_acquire()? {
                return Ok(token);
            }
            waiting();
            self.readable_within(wake)?;
        }
    }

    /// Wait until the FIFO holds a token or `wake` passes, reading nothing: whether it became
    /// readable before then. Another reader may still take the byte first.
    fn readable_within(&self, wake: Duration) -> io::Result<bool> {
        #[cfg(unix)]
        {
            use std::os::fd::AsFd as _;
            let deadline = std::time::Instant::now() + wake;
            Ok(!aterm_uds::ready::any_until(&[self.file.as_fd()], deadline)?.is_empty())
        }
        #[cfg(not(unix))]
        {
            std::thread::sleep(wake);
            Ok(false)
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
    /// waiter needs keep coming back. Across processes the same holds while at most one process
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

impl Drop for Jobserver {
    fn drop(&mut self) {
        if self.created {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// One implicit job. Its lifetime prevents removing the pool while a branch uses it.
#[derive(Debug)]
pub struct Token<'a> {
    pool: &'a Jobserver,
    byte: u8,
}

impl Drop for Token<'_> {
    fn drop(&mut self) {
        let _ = (&self.pool.file).write_all(&[self.byte]);
    }
}

#[cfg(all(test, unix))]
#[path = "jobserver_tests.rs"]
mod tests;
