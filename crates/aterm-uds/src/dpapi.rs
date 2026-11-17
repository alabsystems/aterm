// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! BYTES SEALED TO THIS WINDOWS USER: DPAPI in the user's scope.
//!
//! [`seal`] encrypts bytes with `CryptProtectData` under this Windows user's DPAPI master key and
//! an `entropy` the caller names; [`unseal`] opens them again for the same user with the same
//! entropy. The scope is always the user's: `CRYPTPROTECT_LOCAL_MACHINE`, which would let every
//! account of the machine open the blob, is never passed, and neither call may show anything
//! (`CRYPTPROTECT_UI_FORBIDDEN`). What a blob protects against is a copy of its file read
//! elsewhere: an offline disk, a backup on media with no access lists, another account. Any
//! process this user runs can open it, as any process of a Unix user can read its 0600 file.
//!
//! A blob this user's DPAPI cannot open is [`Unsealed::NotOpenable`]: one sealed by another user,
//! with other entropy, altered since, or sealed under a master key this profile no longer holds
//! (an administrator reset the password, which leaves the old master keys unopenable, or the
//! profile was made again). That is a fact about the blob, not a failed call: asked again, it is
//! refused again. DPAPI answers a logon session that cannot open this user's master keys at all
//! with the same codes (`NTE_BAD_KEY_STATE`): a logon that holds no password, as an OpenSSH key's
//! or a scheduled task's that stores none (S4U), opens no blob and seals none. So a refusal is
//! [`Unsealed::NotOpenable`] only once a fresh round trip in this session (a probe sealed and
//! opened again) shows that DPAPI works here; where the probe fails too, the refusal says nothing
//! of the blob and is [`Unsealed::Failed`], as is any other failure of the call (memory, the
//! security service unreachable), which a later call, or another session, may not repeat.

use std::fmt;
use std::io;
use std::ptr::{null, null_mut};

use crate::secret::{Secret, scrub};
use crate::win::ffi;

/// `CRYPTPROTECT_UI_FORBIDDEN`: the call fails rather than show anything.
const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;

/// `ERROR_INVALID_DATA`: other entropy (none included), or a blob altered since it was sealed
/// (measured, Windows 11 26200).
const ERROR_INVALID_DATA: i32 = 13;
/// `ERROR_INVALID_PARAMETER`: bytes too short to be a blob at all (measured).
const ERROR_INVALID_PARAMETER: i32 = 87;
/// `NTE_BAD_DATA`: a blob DPAPI cannot parse (documented; not met by the tests' alterations).
const NTE_BAD_DATA: i32 = 0x8009_0005_u32.cast_signed();
/// `NTE_BAD_KEY_STATE`: the master key the blob names is not one this user can open, the
/// documented answer after an administrator reset the user's password, and the measured one for
/// a blob naming a master key the profile does not hold.
const NTE_BAD_KEY_STATE: i32 = 0x8009_000B_u32.cast_signed();

/// The bytes and entropy of the probe [`unseal`] seals and opens again before it takes a refusal
/// as a fact about a blob ([`works_here`]): scratch bytes, never a key.
const PROBE: &[u8] = b"aterm-uds dpapi probe: DPAPI seals and opens in this logon session";
const PROBE_ENTROPY: &[u8] = b"aterm-uds dpapi probe entropy";

/// Why [`unseal`] gave no bytes.
#[derive(Debug)]
pub enum Unsealed {
    /// This user's DPAPI cannot open the blob, as it is: another user's, other entropy, altered,
    /// or sealed under a master key this profile no longer holds, while a probe sealed and opened
    /// again in this session shows that DPAPI works here. Asked again, it is refused again.
    NotOpenable(io::Error),
    /// The call failed (memory, the security service, or a logon session in which DPAPI seals
    /// and opens nothing, as one that holds no password): asked again, or in another session, it
    /// may open.
    Failed(io::Error),
}

impl Unsealed {
    /// The call's error, either way.
    #[must_use]
    pub fn into_error(self) -> io::Error {
        match self {
            Self::NotOpenable(error) | Self::Failed(error) => error,
        }
    }

    /// [`Unsealed::NotOpenable`] for the codes that answer a blob this user cannot open, and
    /// [`Unsealed::Failed`] for any other.
    fn of(error: io::Error) -> Self {
        match error.raw_os_error() {
            Some(
                ERROR_INVALID_DATA | ERROR_INVALID_PARAMETER | NTE_BAD_DATA | NTE_BAD_KEY_STATE,
            ) => Self::NotOpenable(error),
            _ => Self::Failed(error),
        }
    }

    /// `self`, a verdict [`Unsealed::of`] gave, held to `works` (a round trip of DPAPI in this
    /// session, [`works_here`]): a refusal stands as [`Unsealed::NotOpenable`] only where
    /// `works` answers that DPAPI seals and opens here, and is [`Unsealed::Failed`], naming both
    /// errors, where it does not. `works` is asked only for a refusal.
    fn confirmed(self, works: impl FnOnce() -> io::Result<()>) -> Self {
        let Self::NotOpenable(error) = self else {
            return self;
        };
        match works() {
            Ok(()) => Self::NotOpenable(error),
            Err(probe) => Self::Failed(io::Error::new(
                error.kind(),
                format!(
                    "{error}, and DPAPI seals and opens nothing in this logon session ({probe}): \
                     a logon that holds no password (an SSH key's, a scheduled task's that stores \
                     none) cannot open this user's master keys, so the refusal says nothing of \
                     the blob"
                ),
            )),
        }
    }
}

/// Whether DPAPI seals and opens again for this user in this logon session: [`PROBE`] sealed,
/// opened, and read back as itself.
///
/// # Errors
/// The seal's or the open's error, or the probe opening to other bytes.
fn works_here() -> io::Result<()> {
    let blob = seal(PROBE, PROBE_ENTROPY)?;
    let opened = opened(&blob, PROBE_ENTROPY)?;
    if opened.expose() != PROBE {
        return Err(io::Error::other("the probe opened to other bytes"));
    }
    Ok(())
}

impl fmt::Display for Unsealed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotOpenable(error) => write!(f, "this Windows user cannot open it: {error}"),
            Self::Failed(error) => write!(f, "DPAPI failed: {error}"),
        }
    }
}

impl std::error::Error for Unsealed {}

/// A blob over `bytes` for a call that only reads it.
fn blob_of(bytes: &[u8]) -> io::Result<ffi::DataBlob> {
    Ok(ffi::DataBlob {
        cbData: u32::try_from(bytes.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "more than 4 GiB to seal"))?,
        pbData: bytes.as_ptr().cast_mut(),
    })
}

/// The output blob DPAPI allocated, freed (wiped first) on drop.
struct Output(ffi::DataBlob);

impl Output {
    const fn new() -> Self {
        Self(ffi::DataBlob {
            cbData: 0,
            pbData: null_mut(),
        })
    }

    /// The bytes DPAPI wrote.
    fn bytes(&mut self) -> &mut [u8] {
        if self.0.pbData.is_null() {
            return &mut [];
        }
        // SAFETY: DPAPI's output buffer holds `cbData` bytes it wrote, owned by this value until
        // drop, and nothing else refers to it.
        unsafe { std::slice::from_raw_parts_mut(self.0.pbData, self.0.cbData as usize) }
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        scrub(self.bytes());
        if !self.0.pbData.is_null() {
            // SAFETY: the buffer is DPAPI's own `LocalAlloc`, freed once, here.
            unsafe { ffi::LocalFree(self.0.pbData.cast()) };
        }
    }
}

/// `bytes` sealed to this Windows user with `entropy` (none where it is empty): a blob only
/// [`unseal`] by this user with the same entropy opens.
///
/// # Errors
/// More than 4 GiB, or `CryptProtectData`'s error (memory, the security service, a logon session
/// that cannot open this user's master keys).
pub fn seal(bytes: &[u8], entropy: &[u8]) -> io::Result<Vec<u8>> {
    #[cfg(any(test, debug_assertions))]
    seam::spend()?;
    let plain = blob_of(bytes)?;
    let entropy_blob = blob_of(entropy)?;
    let entropy_ptr: *const ffi::DataBlob = if entropy.is_empty() {
        null()
    } else {
        &raw const entropy_blob
    };
    let mut out = Output::new();
    // SAFETY: `plain` and `entropy_blob` point at live slices the call only reads; no
    // description, prompt or reserved pointer; `out` receives a buffer `Output` frees. The flags
    // name the user's scope (no CRYPTPROTECT_LOCAL_MACHINE) and forbid UI.
    let ok = unsafe {
        ffi::CryptProtectData(
            &raw const plain,
            null(),
            entropy_ptr,
            null_mut(),
            null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &raw mut out.0,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(out.bytes().to_vec())
}

/// The bytes [`seal`] sealed into `blob` with `entropy` (none where it is empty), for this
/// Windows user.
///
/// # Errors
/// [`Unsealed::NotOpenable`]: this user's DPAPI cannot open the blob as it is (another user's,
/// other entropy, altered, or a master key this profile no longer holds), while DPAPI seals and
/// opens a probe in this session ([`works_here`]). [`Unsealed::Failed`]: the call failed
/// otherwise, a refusal in a session where the probe fails too among them.
pub fn unseal(blob: &[u8], entropy: &[u8]) -> Result<Secret, Unsealed> {
    opened(blob, entropy).map_err(|error| Unsealed::of(error).confirmed(works_here))
}

/// `CryptUnprotectData` of `blob` with `entropy` (none where it is empty), for this Windows user:
/// [`unseal`]'s call, its error as the call gave it.
///
/// # Errors
/// More than 4 GiB, or the call's.
fn opened(blob: &[u8], entropy: &[u8]) -> io::Result<Secret> {
    #[cfg(any(test, debug_assertions))]
    seam::spend()?;
    let sealed = blob_of(blob)?;
    let entropy_blob = blob_of(entropy)?;
    let entropy_ptr: *const ffi::DataBlob = if entropy.is_empty() {
        null()
    } else {
        &raw const entropy_blob
    };
    let mut out = Output::new();
    // SAFETY: as in `seal`; no description is asked for (null), and `out` receives the plain
    // bytes in a buffer `Output` wipes and frees.
    let ok = unsafe {
        ffi::CryptUnprotectData(
            &raw const sealed,
            null_mut(),
            entropy_ptr,
            null_mut(),
            null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &raw mut out.0,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Secret::new(out.bytes()))
}

/// What a test drives that this logon session never shows on its own.
#[cfg(any(test, debug_assertions))]
pub mod seam {
    use std::cell::Cell;
    use std::io;

    thread_local! {
        static LEFT: Cell<Option<u32>> = const { Cell::new(None) };
    }

    /// From now on this thread's DPAPI answers `calls` more calls ([`super::seal`]'s and
    /// [`super::unseal`]'s, the probe's among them) and then fails every one with
    /// `NTE_BAD_KEY_STATE`, as DPAPI does in a logon that holds no password (an OpenSSH key's, a
    /// scheduled task's that stores none); `None` puts it back.
    pub fn answer_then_fail(calls: Option<u32>) {
        LEFT.with(|left| left.set(calls));
    }

    /// One call spent: `NTE_BAD_KEY_STATE` where none is left.
    pub(super) fn spend() -> io::Result<()> {
        LEFT.with(|left| match left.get() {
            None => Ok(()),
            Some(0) => Err(io::Error::from_raw_os_error(super::NTE_BAD_KEY_STATE)),
            Some(n) => {
                left.set(Some(n - 1));
                Ok(())
            }
        })
    }
}

#[cfg(test)]
#[path = "dpapi_tests.rs"]
mod tests;
