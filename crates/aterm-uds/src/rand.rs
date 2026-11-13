// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CSPRNG bytes for token minting: the one audited entropy surface for the workspace.

use std::io;

/// `N` CSPRNG bytes as `2N` lowercase-hex chars: the single-use-token shape.
///
/// # Errors
/// When no OS entropy source is available (see [`fill`]). The caller chooses
/// to fail closed (no token, no socket bind) or degrade with a documented
/// non-secret fallback; it must never retry with its own device read.
pub fn hex_token<const N: usize>() -> io::Result<String> {
    let mut buf = [0u8; N];
    fill(&mut buf)?;
    Ok(hex_encode(&buf))
}

/// Lowercase-hex encode a fixed-size byte array.
#[cfg_attr(trust_verify, trust::skip)] // String allocation can panic; HEX indexing is < 16
pub fn hex_encode<const N: usize>(bytes: &[u8; N]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(N.saturating_mul(2).min(4096));
    for &b in bytes {
        out.push(HEX[usize::from(b.wrapping_shr(4)) & 0xf] as char);
        out.push(HEX[usize::from(b) & 0xf] as char);
    }
    out
}

/// Fill `buf` from the OS CSPRNG.
/// # Errors
/// When no OS entropy source is available.
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
pub fn fill(buf: &mut [u8]) -> io::Result<()> {
    unsafe extern "C" {
        fn getentropy(buf: *mut core::ffi::c_void, len: usize) -> i32;
    }
    fill_from(
        buf,
        |chunk| {
            // `as *mut c_void` rather than `.cast()`, so the verifier sees the slice's
            // provenance.
            // SAFETY: `chunk` is a live &mut [u8] for the call, valid for `chunk.len()`
            // writes; `fill_from` hands over at most 256 bytes, getentropy(2)'s maximum.
            let rc =
                unsafe { getentropy(chunk.as_mut_ptr() as *mut core::ffi::c_void, chunk.len()) };
            rc == 0
        },
        "/dev/urandom",
    )
}

/// The fallback re-reads the whole buffer, and a device that ends early is an error
/// (`read_exact`), never a short fill.
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
fn fill_from(
    buf: &mut [u8],
    entropy: impl FnMut(&mut [u8]) -> bool,
    device: &str,
) -> io::Result<()> {
    if buf.chunks_mut(256).all(entropy) {
        return Ok(());
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    wait_until_rng_ready()?;
    use std::io::Read;
    let mut f = std::fs::File::open(device)?;
    f.read_exact(buf)
}

/// Block until the kernel CSPRNG has been seeded, once per process: the wait
/// `getrandom(2)` does on its own and a `/dev/urandom` read does not.
///
/// Derived from `wait_until_rng_ready` in getrandom 0.2.17 `src/use_file.rs`
/// (Copyright 2018-2024 The rust-random Project Developers; MIT OR
/// Apache-2.0, taken here under Apache-2.0) and modified by the aterm project
/// in 2026: std opens and closes the device, the answer is remembered in an
/// atomic rather than in a cached descriptor, and it compiles on every Unix
/// under test so the poll runs on a macOS box too.
///
/// Once is enough: `/dev/random` turns readable when the pool is first
/// initialised, and before Linux 5.6 it could turn unreadable again as its
/// entropy estimate drained, which `/dev/urandom` never needs to wait for.
#[cfg(all(unix, any(target_os = "linux", target_os = "android", test)))]
#[cfg_attr(trust_verify, trust::skip)]
fn wait_until_rng_ready() -> io::Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
    static READY: AtomicBool = AtomicBool::new(false);
    if READY.load(Relaxed) {
        return Ok(());
    }
    wait_readable("/dev/random")?;
    READY.store(true, Relaxed);
    Ok(())
}

/// Poll `path` for `POLLIN` with no timeout, retrying `EINTR` and `EAGAIN`.
/// Any return of `poll(2)` but -1 is readiness, as upstream reads it (one
/// descriptor and no timeout cannot return 0).
#[cfg(all(unix, any(target_os = "linux", target_os = "android", test)))]
#[cfg_attr(trust_verify, trust::skip)]
fn wait_readable(path: &str) -> io::Result<()> {
    use core::ffi::{c_int, c_short};
    use std::os::fd::AsRawFd;
    /// `struct pollfd`, the same layout on every Unix.
    #[repr(C)]
    struct PollFd {
        fd: c_int,
        events: c_short,
        revents: c_short,
    }
    const POLLIN: c_short = 0x1;
    #[cfg(target_os = "linux")]
    type Nfds = core::ffi::c_ulong;
    #[cfg(not(target_os = "linux"))]
    type Nfds = core::ffi::c_uint;
    unsafe extern "C" {
        fn poll(fds: *mut PollFd, nfds: Nfds, timeout: c_int) -> c_int;
    }
    let device = std::fs::File::open(path)?;
    let mut pfd = PollFd {
        fd: device.as_raw_fd(),
        events: POLLIN,
        revents: 0,
    };
    loop {
        // SAFETY: `pfd` is a live, exclusively borrowed `struct pollfd` for the
        // call and `nfds` is 1; `device` keeps the descriptor open.
        let rc = unsafe { poll(&mut pfd, 1, -1) };
        if rc >= 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        match err.kind() {
            io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock => continue,
            _ => return Err(err),
        }
    }
}

/// Fill `buf` from the OS CSPRNG.
/// # Errors
/// When `BCryptGenRandom` reports failure.
#[cfg(windows)]
#[cfg_attr(trust_verify, trust::skip)]
pub fn fill(buf: &mut [u8]) -> io::Result<()> {
    /// NULL algorithm handle + this flag is the documented system-RNG form.
    const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;
    for chunk in buf.chunks_mut(u32::MAX as usize) {
        // SAFETY: `chunk` is a live &mut [u8] for the duration of the call; 0 == STATUS_SUCCESS.
        let status = unsafe {
            crate::win::ffi::BCryptGenRandom(
                core::ptr::null_mut(),
                chunk.as_mut_ptr(),
                chunk.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status != 0 {
            return Err(io::Error::other(format!(
                "BCryptGenRandom failed (NTSTATUS {status:#010x})"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "rand_tests.rs"]
mod tests;
