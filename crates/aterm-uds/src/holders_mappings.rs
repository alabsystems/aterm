// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! File-backed mappings keep a cache in use after their file descriptors close.

use super::{PathBuf, PathUse, placed};

#[cfg(target_vendor = "apple")]
use super::super::{Failed, clear_errno, ffi};

#[cfg(target_vendor = "apple")]
struct Region {
    address: u64,
    len: u64,
    path: Option<PathBuf>,
}

/// A full walk, or an unreadable process when a record is short, refuses a read, stops
/// advancing, overflows, or exhausts the bound. A matching mapping is already a holder.
#[cfg(target_vendor = "apple")]
fn mac_with(
    pid: u32,
    dirs: &[PathBuf],
    read: impl FnMut(u64) -> Result<Option<Region>, Failed>,
) -> Result<Option<PathUse>, Failed> {
    mac_started(pid, dirs, read, std::time::Instant::now())
}

#[cfg(target_vendor = "apple")]
fn mac_started(
    pid: u32,
    dirs: &[PathBuf],
    mut read: impl FnMut(u64) -> Result<Option<Region>, Failed>,
    started: std::time::Instant,
) -> Result<Option<PathUse>, Failed> {
    let deadline = started + super::super::SCAN_DEADLINE;
    let mut at = 0;
    loop {
        if std::time::Instant::now() >= deadline {
            return Err(Failed::Other);
        }
        let Some(region) = read(at)? else {
            return Ok(None);
        };
        if region.address < at || region.len == 0 {
            return Err(Failed::Other);
        }
        let next = region
            .address
            .checked_add(region.len)
            .ok_or(Failed::Other)?;
        if let Some(path) = region.path.as_deref().and_then(|path| placed(path, dirs)) {
            return Ok(Some(PathUse {
                pid,
                how: "map",
                path,
            }));
        }
        at = next;
    }
}

#[cfg(target_vendor = "apple")]
fn mac_region(buf: &[u8]) -> Result<Region, Failed> {
    use std::os::unix::ffi::OsStrExt as _;
    if buf.len() != ffi::REGION_SIZE {
        return Err(Failed::Other);
    }
    let number = |at| {
        buf.get(at..at + 8)
            .and_then(|bytes| bytes.try_into().ok())
            .map(u64::from_ne_bytes)
            .ok_or(Failed::Other)
    };
    let inode = number(ffi::REGION_INODE)?;
    let bytes = &buf[ffi::REGION_PATH..ffi::REGION_PATH + ffi::MAXPATHLEN];
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(Failed::Other)?;
    let path = if end == 0 {
        if inode != 0 {
            return Err(Failed::Other);
        }
        None
    } else {
        // The kernel forcibly terminates its last byte, so that position may conceal a
        // truncated path. A file-backed record without a complete absolute name is unknown.
        let path = std::path::Path::new(std::ffi::OsStr::from_bytes(&bytes[..end]));
        if end == bytes.len() - 1 || !path.is_absolute() {
            return Err(Failed::Other);
        }
        Some(path.to_path_buf())
    };
    Ok(Region {
        address: number(ffi::REGION_ADDRESS)?,
        len: number(ffi::REGION_LEN)?,
        path,
    })
}

#[cfg(target_vendor = "apple")]
pub(super) fn mac_uses(pid: u32, dirs: &[PathBuf]) -> Result<Option<PathUse>, Failed> {
    let n = i32::try_from(pid).map_err(|_| Failed::Other)?;
    mac_with(pid, dirs, |at| read_mac_region(n, at))
}

#[cfg(target_vendor = "apple")]
fn read_mac_region(n: i32, at: u64) -> Result<Option<Region>, Failed> {
    crate::progress::tick();
    let mut buf = [0u8; ffi::REGION_SIZE];
    clear_errno();
    // SAFETY: `buf` is writable for the SDK-checked record size passed to libproc.
    let got = unsafe {
        ffi::proc_pidinfo(
            n,
            ffi::PROC_PIDREGIONPATHINFO,
            at,
            buf.as_mut_ptr().cast(),
            i32::try_from(buf.len()).map_err(|_| Failed::Other)?,
        )
    };
    let error = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    mac_answer(got, error, &buf)
}

#[cfg(target_vendor = "apple")]
fn mac_answer(got: i32, error: i32, buf: &[u8]) -> Result<Option<Region>, Failed> {
    if got <= 0 {
        // XNU's region reader returns EINVAL when no region exists at or after the
        // requested address. ESRCH still means the process exited, not a complete walk.
        return if error == libc::EINVAL {
            Ok(None)
        } else {
            Err(Failed::of(error))
        };
    }
    let len = usize::try_from(got).map_err(|_| Failed::Other)?;
    mac_region(buf.get(..len).ok_or(Failed::Other)?).map(Some)
}

#[cfg(any(target_os = "linux", all(test, unix)))]
mod procfs {
    use super::{PathBuf, PathUse, placed};
    use std::io::Read as _;
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::Path;

    const MAX_BYTES: usize = 16 << 20;

    fn number(bytes: &[u8], radix: u32) -> Result<u64, ()> {
        let valid = match radix {
            10 => bytes.iter().all(u8::is_ascii_digit),
            16 => bytes.iter().all(u8::is_ascii_hexdigit),
            _ => false,
        };
        if !valid {
            return Err(());
        }
        u64::from_str_radix(std::str::from_utf8(bytes).map_err(|_| ())?, radix).map_err(|_| ())
    }

    fn pair(bytes: &[u8], separator: u8) -> Result<(&[u8], &[u8]), ()> {
        let at = bytes.iter().position(|byte| *byte == separator).ok_or(())?;
        Ok((&bytes[..at], &bytes[at + 1..]))
    }

    /// `/proc/pid/maps` escapes newline as `\012`; a literal backslash followed by `012`
    /// is ambiguous. Encode each known root rather than guessing every combination of
    /// decoded path bytes. A witness retains the real root spelling for its consumers.
    fn mapped_path(bytes: &[u8], dirs: &[PathBuf]) -> Result<Option<PathBuf>, ()> {
        fn data_path(bytes: &[u8]) -> &[u8] {
            match bytes.strip_prefix(b"/System/Volumes/Data") {
                Some([]) => b"/",
                Some(rest) if rest.starts_with(b"/") => rest,
                _ => bytes,
            }
        }
        let bytes = data_path(bytes);
        let mut matching = Vec::new();
        for dir in dirs {
            let normalized: PathBuf = dir.components().collect();
            let mut encoded = Vec::new();
            for byte in data_path(normalized.as_os_str().as_bytes()) {
                if *byte == b'\n' {
                    encoded.extend_from_slice(b"\\012");
                } else {
                    encoded.push(*byte);
                }
            }
            if let Some(rest) = bytes.strip_prefix(encoded.as_slice()) {
                if rest.is_empty() {
                    matching.push((dir, dir.clone()));
                } else if let Some(relative) = if encoded == b"/" {
                    Some(rest)
                } else {
                    rest.strip_prefix(b"/")
                } {
                    matching.push((dir, dir.join(std::ffi::OsStr::from_bytes(relative))));
                }
            }
        }
        if matching.is_empty() {
            return Ok(None);
        }
        // A batch consumer must be able to place the witness under every possible owner.
        // Nested roots have such a witness; mutually ambiguous spellings remain unknown.
        matching
            .iter()
            .map(|(_, witness)| witness)
            .find(|witness| {
                matching
                    .iter()
                    .all(|(dir, _)| placed(witness, std::slice::from_ref(*dir)).is_some())
            })
            .cloned()
            .map(Some)
            .ok_or(())
    }

    fn parse(bytes: &[u8], pid: u32, dirs: &[PathBuf]) -> Result<Option<PathUse>, ()> {
        if bytes.len() > MAX_BYTES || (!bytes.is_empty() && !bytes.ends_with(b"\n")) {
            return Err(());
        }
        let mut previous = 0;
        if bytes.is_empty() {
            return Ok(None);
        }
        for line in bytes[..bytes.len() - 1].split(|byte| *byte == b'\n') {
            let mut tail = line;
            let mut fields: [&[u8]; 5] = [&[]; 5];
            for field in &mut fields {
                let start = tail
                    .iter()
                    .position(|byte| !byte.is_ascii_whitespace())
                    .ok_or(())?;
                tail = &tail[start..];
                let end = tail
                    .iter()
                    .position(u8::is_ascii_whitespace)
                    .unwrap_or(tail.len());
                *field = &tail[..end];
                tail = &tail[end..];
            }
            let (start, end) = pair(fields[0], b'-')?;
            let (start, end) = (number(start, 16)?, number(end, 16)?);
            if start < previous || end <= start {
                return Err(());
            }
            previous = end;
            if !matches!(
                fields[1],
                [b'r' | b'-', b'w' | b'-', b'x' | b'-', b'p' | b's']
            ) {
                return Err(());
            }
            number(fields[2], 16)?;
            let (major, minor) = pair(fields[3], b':')?;
            number(major, 16)?;
            number(minor, 16)?;
            let inode = number(fields[4], 10)?;
            let start = tail
                .iter()
                .position(|byte| !byte.is_ascii_whitespace())
                .unwrap_or(tail.len());
            let name = &tail[start..];
            if name.contains(&0) {
                return Err(());
            }
            if name.starts_with(b"/") {
                if let Some(path) = mapped_path(name, dirs)? {
                    return Ok(Some(PathUse {
                        pid,
                        how: "map",
                        path,
                    }));
                }
            } else if inode != 0
                || (!name.is_empty() && !(name.starts_with(b"[") && name.ends_with(b"]")))
            {
                return Err(());
            }
        }
        Ok(None)
    }

    pub(super) fn uses(proc_dir: &Path, pid: u32, dirs: &[PathBuf]) -> Result<Option<PathUse>, ()> {
        let bytes = (|| -> std::io::Result<Vec<u8>> {
            use std::os::unix::fs::OpenOptionsExt as _;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW)
                .open(proc_dir.join("maps"))?;
            if !file.metadata()?.is_file() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "not a regular file",
                ));
            }
            let mut bytes = Vec::new();
            file.take((MAX_BYTES + 1) as u64).read_to_end(&mut bytes)?;
            Ok(bytes)
        })();
        match bytes {
            Ok(bytes) => parse(&bytes, pid, dirs),
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && std::fs::symlink_metadata(proc_dir)
                        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                Ok(None)
            }
            Err(_) => Err(()),
        }
    }

    #[cfg(test)]
    mod tests;
}

#[cfg(target_os = "linux")]
pub(super) fn linux_uses(
    proc_dir: &std::path::Path,
    pid: u32,
    dirs: &[PathBuf],
) -> Result<Option<PathUse>, ()> {
    procfs::uses(proc_dir, pid, dirs)
}

#[cfg(all(test, target_vendor = "apple"))]
#[path = "holders_mappings_mac_tests.rs"]
mod mac_tests;
