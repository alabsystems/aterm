// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CSPRNG bytes for token minting: the one audited entropy surface for the workspace.

use std::io;

/// `N` CSPRNG bytes as `2N` lowercase-hex chars: the single-use-token shape.
/// # Errors
/// When no OS entropy source is available (see [`fill`]).
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
            // SAFETY: `chunk` is a live &mut [u8] for the call, valid for `chunk.len()`
            let rc =
                unsafe { getentropy(chunk.as_mut_ptr() as *mut core::ffi::c_void, chunk.len()) };
            rc == 0
        },
        "/dev/urandom",
    )
}

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
        // SAFETY: pfd is a live, exclusively borrowed struct pollfd for the call and nfds.
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
mod tests {
    use super::hex_encode;

    /// and a longer input is the concatenation of its bytes' pairs.
    #[test]
    fn hex_encode_is_exact_lowercase_and_injective() {
        let pair = |b: u8| hex_encode(&[b]);
        let mut seen = std::collections::BTreeSet::new();
        for b in 0..=u8::MAX {
            let hex = pair(b);
            assert_eq!(hex.len(), 2, "{b:#04x}");
            assert!(
                hex.bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
                "{b:#04x} -> {hex:?} is not lowercase hex"
            );
            assert_eq!(u8::from_str_radix(&hex, 16), Ok(b), "{hex:?} decodes back");
            assert!(seen.insert(hex), "{b:#04x} prints like another byte");
        }
        assert_eq!(hex_encode(&[]), "");
        fn concat<const N: usize>() {
            for pos in 0..N {
                for b in 0..=u8::MAX {
                    let mut bytes = [0x5a; N];
                    bytes[pos] = b;
                    let want: String = bytes.iter().map(|&x| hex_encode(&[x])).collect();
                    assert_eq!(want.len(), 2 * N);
                    assert_eq!(hex_encode(&bytes), want, "N={N} pos={pos} b={b:#04x}");
                }
            }
        }
        concat::<8>();
        concat::<16>();
    }

    /// that fills everything leaves the (nonexistent) device unopened.
    #[cfg(unix)]
    #[test]
    fn the_source_is_asked_in_chunks_of_at_most_256_and_the_device_is_left_alone() {
        let mut asked = Vec::new();
        let mut buf = [0u8; 600];
        let r = super::fill_from(
            &mut buf,
            |chunk| {
                asked.push(chunk.len());
                chunk.fill(0x5a);
                true
            },
            "/nonexistent/aterm-uds-rand-device",
        );
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(asked, [256, 256, 88]);
        assert!(buf.iter().all(|&b| b == 0x5a));
        let mut empty = [0u8; 0];
        let r = super::fill_from(&mut empty, |_| false, "/nonexistent/aterm-uds-rand-device");
        assert!(r.is_ok(), "an empty buffer asks nothing: {r:?}");
    }

    /// fallback that succeeds (`/dev/zero`) replaces the whole buffer.
    #[cfg(unix)]
    #[test]
    fn a_forced_failure_is_an_error_and_the_fallback_rewrites_the_whole_buffer() {
        let mut buf = [0xa5u8; 600];
        let r = super::fill_from(&mut buf, |_| false, "/nonexistent/aterm-uds-rand-device");
        assert_eq!(r.map_err(|e| e.kind()), Err(std::io::ErrorKind::NotFound));

        let mut first = true;
        let mut buf = [0xa5u8; 600];
        let r = super::fill_from(
            &mut buf,
            |chunk| {
                chunk.fill(0x11);
                std::mem::replace(&mut first, false)
            },
            "/dev/null",
        );
        assert_eq!(
            r.map_err(|e| e.kind()),
            Err(std::io::ErrorKind::UnexpectedEof)
        );

        let mut first = true;
        let mut buf = [0xa5u8; 600];
        let r = super::fill_from(
            &mut buf,
            |chunk| {
                chunk.fill(0x11);
                std::mem::replace(&mut first, false)
            },
            "/dev/zero",
        );
        assert!(r.is_ok(), "{r:?}");
        assert!(
            buf.iter().all(|&b| b == 0),
            "the device's bytes, all 600 of them"
        );
    }

    /// on every Unix under test, so this runs on macOS too.
    #[cfg(unix)]
    #[test]
    fn the_readiness_poll_returns_on_dev_random_and_reports_a_missing_device() {
        assert!(super::wait_readable("/dev/random").is_ok());
        assert!(super::wait_until_rng_ready().is_ok());
        assert!(
            super::wait_until_rng_ready().is_ok(),
            "the remembered answer"
        );
        let r = super::wait_readable("/nonexistent/aterm-uds-rand-device");
        assert_eq!(r.map_err(|e| e.kind()), Err(std::io::ErrorKind::NotFound));
    }
}
