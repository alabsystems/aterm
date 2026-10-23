// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `KERN_PROCARGS2`, the kernel's record of what a process was exec'd with: the workspace's one
//! reader of it ([`read`], macOS) and one parser ([`parse`]).

/// One `KERN_PROCARGS2` answer, parsed ([`parse`]): views into its buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcArgs2<'a> {
    /// The path the process was exec'd from, as `exec` was given it: not canonical, possibly empty,
    /// not always UTF-8.
    pub exec_path: &'a [u8],
    /// The `argc` the kernel recorded.
    pub argc: usize,
    /// The argument words, `argv[0]` first: each NUL-terminated word the buffer holds, at most
    /// [`Self::argc`].
    pub argv: Vec<&'a [u8]>,
    /// What follows the last word read: the environment, when whole.
    after: &'a [u8],
}

impl<'a> ProcArgs2<'a> {
    /// Whether every one of [`Self::argc`] words was read: the buffer was not cut inside its
    /// argument words.
    #[must_use]
    pub const fn whole(&self) -> bool {
        self.argv.len() == self.argc
    }

    /// The initial environment's `KEY=value` strings, in the kernel's order: each NUL-terminated
    /// string after the argument words, up to the first empty one.
    pub fn env(&self) -> impl Iterator<Item = &'a [u8]> + use<'a> {
        let mut rest: &'a [u8] = if self.whole() { self.after } else { &[] };
        std::iter::from_fn(move || {
            let end = rest.iter().position(|b| *b == 0)?;
            let word = &rest[..end];
            rest = &rest[end + 1..];
            (!word.is_empty()).then_some(word)
        })
    }
}

/// The bytes of the native-endian `c_int` argc a `KERN_PROCARGS2` buffer leads
/// with.
const ARGC_BYTES: usize = 4;

/// Parse one `KERN_PROCARGS2` answer ([`ProcArgs2`]).
#[must_use]
pub fn parse(buf: &[u8]) -> Option<ProcArgs2<'_>> {
    let argc = i32::from_ne_bytes(buf.get(..ARGC_BYTES)?.try_into().ok()?);
    let argc = usize::try_from(argc).ok()?;
    let rest = &buf[ARGC_BYTES..];
    let path_end = rest.iter().position(|b| *b == 0)?;
    let exec_path = &rest[..path_end];
    let rest = &rest[path_end..];
    let first = rest.iter().position(|b| *b != 0).unwrap_or(rest.len());
    let mut after = &rest[first..];
    let mut argv = Vec::with_capacity(argc.min(1024));
    while argv.len() < argc {
        let Some(end) = after.iter().position(|b| *b == 0) else {
            break;
        };
        argv.push(&after[..end]);
        after = &after[end + 1..];
    }
    Some(ProcArgs2 {
        exec_path,
        argc,
        argv,
        after,
    })
}

/// `KERN_PROCARGS2` (`<sys/sysctl.h>`): a process's argc, exec path, argument words and initial
/// environment.
#[cfg(target_os = "macos")]
pub const KERN_PROCARGS2: libc::c_int = 49;

/// `KERN_ARGMAX` (`<sys/sysctl.h>`): the most bytes an exec's arguments and environment may take,
/// and, past the argc, the most of a `KERN_PROCARGS2` record the kernel hands over ([`read`]).
#[cfg(target_os = "macos")]
pub const KERN_ARGMAX: libc::c_int = 8;

/// `pid`'s `KERN_PROCARGS2` answer, whole: read once into the largest buffer the kernel takes,
/// `kern.argmax` bytes past the argc (one byte more is `EINVAL`).
#[must_use]
pub fn read(pid: u32) -> Option<Vec<u8>> {
    imp::read(pid)
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{KERN_ARGMAX, KERN_PROCARGS2};
    use std::sync::OnceLock;

    /// `kern.argmax`, read once: a constant of the running kernel.
    #[cfg_attr(trust_verify, trust::skip)]
    fn argmax() -> Option<usize> {
        static ARGMAX: OnceLock<Option<usize>> = OnceLock::new();
        *ARGMAX.get_or_init(|| {
            let mut argmax: libc::c_int = 0;
            let mut len = std::mem::size_of::<libc::c_int>();
            let mut mib = [libc::CTL_KERN, KERN_ARGMAX];
            // SAFETY: `mib` names a two-integer key; `argmax` and `len` are ours and outlive the
            // call; nothing is written (`newp` null).
            let rc = unsafe {
                libc::sysctl(
                    mib.as_mut_ptr(),
                    2,
                    (&raw mut argmax).cast::<libc::c_void>(),
                    &raw mut len,
                    std::ptr::null_mut(),
                    0,
                )
            };
            usize::try_from(argmax)
                .ok()
                .filter(|n| rc == 0 && len == std::mem::size_of::<libc::c_int>() && *n > 4)
        })
    }

    #[cfg_attr(trust_verify, trust::skip)]
    pub(super) fn read(pid: u32) -> Option<Vec<u8>> {
        let pid = libc::c_int::try_from(pid).ok().filter(|p| *p > 0)?;
        let cap = argmax()?.checked_add(super::ARGC_BYTES)?;
        let mut buf = vec![0u8; cap];
        let mut len = cap;
        let mut mib = [libc::CTL_KERN, KERN_PROCARGS2, pid];
        // SAFETY: `buf` is `cap` writable bytes and `len` says so; the kernel writes at most `len`
        // and sets it to what it wrote; nothing is written to the kernel (`newp` null).
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                buf.as_mut_ptr().cast::<libc::c_void>(),
                &raw mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || len >= cap {
            return None;
        }
        buf.truncate(len);
        Some(buf)
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub(super) fn read(_pid: u32) -> Option<Vec<u8>> {
        None
    }
}

#[cfg(test)]
#[path = "procargs_tests.rs"]
mod tests;
