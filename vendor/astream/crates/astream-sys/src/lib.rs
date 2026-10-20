// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! # astream-sys
//!
//! The durable log's OS cordon: the two things `astream-broker` needs from the
//! kernel that safe `std` does not offer.
//!
//! - [`barrier_sync`]: an ORDERING-only flush of a file. On Apple it is
//!   `fcntl(fd, F_BARRIERFSYNC)`, which fcntl(2) defines device-wide: every write
//!   issued before it reaches stable media before any write issued after it, without
//!   waiting for the drive to empty its cache — the cost `F_FULLFSYNC` pays. Anywhere
//!   else it is `sync_data`, a FULL flush: Linux has no ordering-only primitive
//!   (`fdatasync` flushes the drive cache), so the fallback is never weaker than asked.
//!
//!   What the two cost was measured once, on ONE Mac (Apple SSD, APFS), a shared
//!   development machine under other agents' builds and Strict brokers (load averages
//!   2–48), by a scratch probe that is not in this repository — so these are that
//!   machine's numbers on those days, not a bound. A single writer, write + flush, in
//!   a tight loop: a 64-byte append took p50 0.14 ms, p95 1.1 ms, p99 5.1 ms with
//!   `F_BARRIERFSYNC`, and p50 7.3 ms, p95 17 ms, p99 21 ms with `F_FULLFSYNC` (n =
//!   6,000 each, three passes pooled); a 4 KiB append p50 0.49 / p99 2.3 ms against
//!   p50 4.3 / p99 8.6 ms (n = 6,000 each). The flush's own floor, every single-writer
//!   pass pooled: `F_BARRIERFSYNC` min 94 µs, p50 294 µs (n = 26,000); `F_FULLFSYNC` min
//!   1.8 ms, p50 4.3 ms (n = 42,000), in steps of about 4.2 ms when the device is
//!   already flushing for someone else. The tiers that use the barrier measure their
//!   own cost before they become a default.
//! - [`boot_session`]: THIS BOOT of the machine as the kernel names it, as 16 bytes.
//!   Two readings are equal exactly when no boot came between them, which is the
//!   question a log that trusts the page cache asks — "has the kernel holding my
//!   unflushed writes run throughout?". A process crash, a restart, a sleep or a clock
//!   step leave it as it was (the page cache outlives all of them); a reboot, a kernel
//!   panic or a power loss give a new one. Apple: `kern.bootsessionuuid`. Linux:
//!   `/proc/sys/kernel/random/boot_id` (safe `std`). Elsewhere `None`, which the
//!   broker reads as "a boot it cannot vouch for": the safe direction.
//!
//! ## The cordon
//!
//! The crate root is `deny(unsafe_code)`, and exactly one module re-permits it:
//! `apple` (`cfg(target_vendor = "apple")`), two hand-declared FFI calls with a
//! `// SAFETY:` note each. Nothing else here may use `unsafe` — the compiler refuses
//! it — and no errno or raw pointer leaves that module: every call returns
//! `io::Result` or an owned value. `astream-broker` stays `forbid(unsafe_code)` and
//! reaches the OS through this crate's two safe functions (and reads a barrier's
//! refusal through [`barrier_refused`], plain errno numbers that need no `unsafe`).

#![deny(unsafe_code)]

use std::fs::File;
use std::io;

#[cfg(target_vendor = "apple")]
macro_rules! apple_ffi {
    (barrier_fsync($fd:expr, $cmd:expr)) => {
        // SAFETY: `fd` is a valid open descriptor owned by `file` and `F_BARRIERFSYNC` takes no extra arguments.
        unsafe { fcntl($fd, $cmd) }
    };
    (sysctlbyname($name:expr, $oldp:expr, $oldlenp:expr, $newp:expr, $newlen:expr)) => {
        // SAFETY: NUL-terminated name, writable buffer, and length pointer are valid for the duration of the call.
        unsafe { sysctlbyname($name, $oldp, $oldlenp, $newp, $newlen) }
    };
}

#[cfg(target_vendor = "apple")]
#[allow(unsafe_code)]
mod apple;

#[macro_use]
#[path = "../../astream-wire/contract_fn.rs"]
mod contract_fn;

#[cfg(any(test, not(clean_islands)))]
mod twins;

#[cfg(clean_islands)]
#[path = "boot_island.rs"]
mod boot_island;
#[cfg(clean_islands)]
pub use boot_island::boot_nibble_advance;
#[cfg(not(clean_islands))]
pub use twins::boot_nibble_advance;

contract_fn! {
    /// Verified check that a parsed boot session UUID has exactly 32 hex nibbles (`16` bytes),
    /// linked via `clean-kernel-defeq` to `boot_nibble_count_valid_isl`.
    #[must_use]
    pub const fn boot_nibble_count_valid(nibbles: usize) -> bool
    clauses [ensures result == (nibbles == 32)]
    {
        nibbles == 32
    }
}

#[must_use]
pub const fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0' => Some(0),
        b'1' => Some(1),
        b'2' => Some(2),
        b'3' => Some(3),
        b'4' => Some(4),
        b'5' => Some(5),
        b'6' => Some(6),
        b'7' => Some(7),
        b'8' => Some(8),
        b'9' => Some(9),
        b'a' | b'A' => Some(10),
        b'b' | b'B' => Some(11),
        b'c' | b'C' => Some(12),
        b'd' | b'D' => Some(13),
        b'e' | b'E' => Some(14),
        b'f' | b'F' => Some(15),
        _ => None,
    }
}

/// Pack a 16-byte boot session UUID into two big-endian `u64` words `(hi, lo)`
/// using fixed-width bitwise shifts.
#[must_use]
pub const fn encode_boot_id_u128_be(id: [u8; 16]) -> (u64, u64) {
    let hi = ((id[0] as u64) << 56)
        | ((id[1] as u64) << 48)
        | ((id[2] as u64) << 40)
        | ((id[3] as u64) << 32)
        | ((id[4] as u64) << 24)
        | ((id[5] as u64) << 16)
        | ((id[6] as u64) << 8)
        | (id[7] as u64);
    let lo = ((id[8] as u64) << 56)
        | ((id[9] as u64) << 48)
        | ((id[10] as u64) << 40)
        | ((id[11] as u64) << 32)
        | ((id[12] as u64) << 24)
        | ((id[13] as u64) << 16)
        | ((id[14] as u64) << 8)
        | (id[15] as u64);
    (hi, lo)
}

/// Unpack two big-endian `u64` words `(hi, lo)` into a 16-byte boot session UUID
/// using fixed-width bitwise shifts.
#[must_use]
pub const fn decode_boot_id_u128_be(hi: u64, lo: u64) -> [u8; 16] {
    [
        ((hi >> 56) & 0xFF) as u8,
        ((hi >> 48) & 0xFF) as u8,
        ((hi >> 40) & 0xFF) as u8,
        ((hi >> 32) & 0xFF) as u8,
        ((hi >> 24) & 0xFF) as u8,
        ((hi >> 16) & 0xFF) as u8,
        ((hi >> 8) & 0xFF) as u8,
        (hi & 0xFF) as u8,
        ((lo >> 56) & 0xFF) as u8,
        ((lo >> 48) & 0xFF) as u8,
        ((lo >> 40) & 0xFF) as u8,
        ((lo >> 32) & 0xFF) as u8,
        ((lo >> 24) & 0xFF) as u8,
        ((lo >> 16) & 0xFF) as u8,
        ((lo >> 8) & 0xFF) as u8,
        (lo & 0xFF) as u8,
    ]
}

/// Whether a Darwin `errno` value (`ENODEV=19`, `EINVAL=22`, `ENOTTY=25`, `ENOTSUP=45`)
/// indicates that the filesystem or device refuses `F_BARRIERFSYNC`.
#[must_use]
pub const fn is_apple_barrier_refused_errno(errno: i32) -> bool {
    matches!(errno, 19 | 22 | 25 | 45)
}

/// Order every write to `file` issued before this call ahead of every write issued
/// after it, on stable media — without waiting for the media to hold either.
///
/// Apple: `fcntl(fd, F_BARRIERFSYNC)`, retried on `EINTR`. A filesystem or device
/// that cannot honour the barrier answers an error (`ENOTSUP`, `ENODEV`, `EINVAL` or
/// `ENOTTY`), which the caller must treat as "no ordering was promised" — the broker
/// then flushes in full instead. Elsewhere: [`File::sync_data`], a full flush, which
/// orders everything because it persists everything.
pub fn barrier_sync(file: &File) -> io::Result<()> {
    #[cfg(target_vendor = "apple")]
    {
        apple::barrier_fsync(file)
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        file.sync_data()
    }
}

/// Whether `err`, from [`barrier_sync`], says the file or its device cannot take a
/// barrier at all — `ENOTSUP`, `ENODEV`, `EINVAL` or `ENOTTY` on Apple — rather than
/// that a flush failed. The caller then knows no ordering was promised and the bytes
/// are still only in the page cache: it flushes in full instead, and serves the
/// stronger tier from then on. Any other error is a failed flush, to be handled as a
/// failed `fsync` is.
///
/// Always `false` off Apple: there [`barrier_sync`] IS a full flush, so any error it
/// returns is a failed flush.
#[must_use]
pub fn barrier_refused(err: &io::Error) -> bool {
    // The errno values of <sys/errno.h> on Apple platforms (Darwin's own numbering:
    // ENOTSUP is 45 there, not Linux's 95). Plain numbers, not FFI: nothing here
    // crosses into the kernel, so the cordon's `unsafe` stays in `apple`.
    #[cfg(target_vendor = "apple")]
    {
        match err.raw_os_error() {
            Some(errno) => is_apple_barrier_refused_errno(errno),
            None => false,
        }
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let _ = err;
        false
    }
}

/// This boot's id as 16 bytes, or `None` where this host cannot read one (or reads
/// something that is not a boot id: see [`parse_boot_id`]).
#[must_use]
pub fn boot_session() -> Option<[u8; 16]> {
    #[cfg(target_vendor = "apple")]
    {
        match apple::boot_session_uuid() {
            Some(buf) => parse_boot_id(&buf),
            None => None,
        }
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let raw = match std::fs::read("/proc/sys/kernel/random/boot_id") {
            Ok(b) => b,
            Err(_) => return None,
        };
        parse_boot_id(&raw)
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
    {
        None
    }
}

#[must_use]
pub const fn nul_offset(raw: &[u8]) -> usize {
    let mut i = 0usize;
    let mut scan = raw;
    while let &[b, ref tail @ ..] = scan {
        if b == 0 {
            return i;
        }
        i = i.saturating_add(1);
        scan = tail;
    }
    raw.len()
}

/// Call-free `const fn` scanner that parses a NUL-terminated or whitespace-framed
/// ASCII UUID into two non-zero big-endian `u64` words `(hi, lo)`.
#[must_use]
pub const fn scan_boot_id_words(raw: &[u8]) -> Option<(u64, u64)> {
    let mut scan = raw;
    let mut hi = 0u64;
    let mut lo = 0u64;
    let mut nibbles = 0usize;
    let mut phase = 0u8;
    let mut skip = 0u8;
    while let &[b, ref tail @ ..] = scan {
        if skip > 0 {
            skip = (skip & 3).wrapping_sub(1);
            scan = tail;
            continue;
        }
        if b == 0 {
            break;
        }
        if matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C) {
            if phase == 1 {
                phase = 2;
            }
            scan = tail;
            continue;
        }
        if b == 0xC2 {
            if matches!(tail, &[0x85 | 0xA0, ..]) {
                if phase == 1 {
                    phase = 2;
                }
                skip = 1;
                scan = tail;
                continue;
            }
            return None;
        }
        if matches!(b, 0xE1..=0xE3) {
            if let &[b2, b3, ..] = tail {
                let is_ws = (b == 0xE1 && b2 == 0x9A && b3 == 0x80)
                    || (b == 0xE2
                        && b2 == 0x80
                        && ((b3 >= 0x80 && b3 <= 0x8A) || b3 == 0xA8 || b3 == 0xA9 || b3 == 0xAF))
                    || (b == 0xE2 && b2 == 0x81 && b3 == 0x9F)
                    || (b == 0xE3 && b2 == 0x80 && b3 == 0x80);
                if is_ws {
                    if phase == 1 {
                        phase = 2;
                    }
                    skip = 2;
                    scan = tail;
                    continue;
                }
            }
            return None;
        }
        if phase == 2 {
            return None;
        }
        phase = 1;
        if b == b'-' {
            scan = tail;
            continue;
        }
        let v: u64 = match b {
            b'0' => 0,
            b'1' => 1,
            b'2' => 2,
            b'3' => 3,
            b'4' => 4,
            b'5' => 5,
            b'6' => 6,
            b'7' => 7,
            b'8' => 8,
            b'9' => 9,
            b'a' | b'A' => 10,
            b'b' | b'B' => 11,
            b'c' | b'C' => 12,
            b'd' | b'D' => 13,
            b'e' | b'E' => 14,
            b'f' | b'F' => 15,
            _ => return None,
        };
        if nibbles < 16 {
            hi = (hi << 4) | v;
        } else if nibbles < 32 {
            lo = (lo << 4) | v;
        } else {
            return None;
        }
        nibbles = nibbles.saturating_add(1);
        scan = tail;
    }
    if nibbles == 32 && (hi | lo) != 0 {
        Some((hi, lo))
    } else {
        None
    }
}

/// The kernel's boot id text as 16 bytes: the bytes up to a NUL, whitespace trimmed,
/// then exactly 32 hexadecimal digits once the hyphens of the UUID form are dropped
/// (either case). Anything else — empty, too short or long, a stray character — is
/// `None`, and so is the all-zero id, which a log's identity sidecar reserves for "the
/// boot is unknown": a damaged answer must never pass as a boot, because two equal
/// damaged answers would read as "no reboot came between them".
#[must_use]
#[allow(clippy::question_mark)]
pub fn parse_boot_id(raw: &[u8]) -> Option<[u8; 16]> {
    let Some((hi, lo)) = scan_boot_id_words(raw) else {
        return None;
    };
    if boot_nibble_count_valid(32) {
        Some(decode_boot_id_u128_be(hi, lo))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "64C7D12D-5B0E-4F43-9A1C-2E7F00D1A9B3";

    #[test]
    fn a_uuid_parses_in_either_case_with_or_without_its_terminator() {
        let want = [
            0x64, 0xc7, 0xd1, 0x2d, 0x5b, 0x0e, 0x4f, 0x43, 0x9a, 0x1c, 0x2e, 0x7f, 0x00, 0xd1,
            0xa9, 0xb3,
        ];
        assert_eq!(parse_boot_id(ID.as_bytes()), Some(want));
        assert_eq!(parse_boot_id(format!("{ID}\0\0\0").as_bytes()), Some(want));
        assert_eq!(
            parse_boot_id(format!("{}\n", ID.to_lowercase()).as_bytes()),
            Some(want)
        );
        let (hi, lo) = encode_boot_id_u128_be(want);
        assert_eq!(decode_boot_id_u128_be(hi, lo), want);
        assert_eq!(scan_boot_id_words(ID.as_bytes()), Some((hi, lo)));
        assert_eq!(nul_offset(b"abc\0def"), 3);
        assert_eq!(hex_nibble(b'f'), Some(15));

        // The public reading keeps its UTF-8 whitespace contract. The kernel's
        // normal ASCII spelling and bytes beyond its first NUL remain ordinary.
        assert_eq!(
            parse_boot_id(format!(" \t\r\n{ID}\x0b\x0c ").as_bytes()),
            Some(want)
        );
        let mut invalid_prefix = vec![0xff];
        invalid_prefix.extend_from_slice(ID.as_bytes());
        assert_eq!(parse_boot_id(&invalid_prefix), None);
        let mut after_nul = format!("{ID}\0").into_bytes();
        after_nul.extend_from_slice(&[0xff, 0xfe]);
        assert_eq!(parse_boot_id(&after_nul), Some(want));
        let internal = format!("{}\u{a0}{}", &ID[..8], &ID[8..]);
        assert_eq!(parse_boot_id(internal.as_bytes()), None);
        for (left, right) in [
            ("\u{a0}", "\u{2003}"),
            ("\u{85}", "\u{3000}"),
            ("\u{202f}", "\u{205f}"),
            ("\u{2028}", "\u{2029}"),
        ] {
            let framed = format!("{left}{ID}{right}");
            assert_eq!(
                parse_boot_id(framed.as_bytes()),
                Some(want),
                "PUBLIC_BOOT_ID_TRIMS_VALID_UTF8_WHITESPACE: {framed:?}"
            );
        }
    }

    #[test]
    fn anything_but_a_boot_id_is_none() {
        for bad in [
            &b""[..],
            b"\0",
            b"   ",
            b"64C7D12D-5B0E-4F43-9A1C-2E7F00D1A9B", // one digit short
            b"64C7D12D-5B0E-4F43-9A1C-2E7F00D1A9B3A", // one digit long
            b"64C7D12D-5B0E-4F43-9A1C-2E7F00D1A9BZ", // not hex
            b"00000000-0000-0000-0000-000000000000", // the reserved unknown
            b"\xff\xfe",
        ] {
            assert_eq!(parse_boot_id(bad), None, "{bad:?}");
        }
    }

    /// The live reading, where this host has one: stable within a process (no boot can
    /// come between two calls of one test), and never the reserved all-zero id.
    #[test]
    fn the_live_boot_session_is_stable_within_a_process() {
        let first = boot_session();
        assert_eq!(first, boot_session());
        assert_ne!(first, Some([0u8; 16]));
        #[cfg(any(target_vendor = "apple", target_os = "linux"))]
        assert!(first.is_some(), "this host's kernel names its boot");
    }

    /// The four errnos fcntl(2) answers for a file or device that cannot order its
    /// writes are a refusal on Apple; a real I/O failure, or any error off Apple (where
    /// the barrier is a full flush), is not.
    #[test]
    fn only_the_devices_refusal_reads_as_a_refused_barrier() {
        let refused = [19, 22, 25, 45].map(io::Error::from_raw_os_error);
        let failed = [5, 28].map(io::Error::from_raw_os_error); // EIO, ENOSPC
        for e in &refused {
            assert_eq!(barrier_refused(e), cfg!(target_vendor = "apple"), "{e}");
        }
        for e in &failed {
            assert!(!barrier_refused(e), "{e}");
        }
        assert!(!barrier_refused(&io::Error::other("not an errno")));
    }

    #[test]
    fn a_barrier_sync_of_a_regular_file_succeeds() {
        let dir = std::env::temp_dir().join(format!("astream-sys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("barrier");
        let mut f = File::create(&path).unwrap();
        std::io::Write::write_all(&mut f, b"ordered").unwrap();
        let res = barrier_sync(&f);
        let _ = std::fs::remove_dir_all(&dir);
        res.expect("a regular file on the test volume takes a barrier");
    }
}
