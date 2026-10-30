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
    mut read: impl FnMut(u64) -> Result<Option<Region>, Failed>,
) -> Result<Option<PathUse>, Failed> {
    let mut at = 0;
    for _ in 0..65_536 {
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
    Err(Failed::Other)
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
    mac_with(pid, dirs, |at| {
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
    })
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
            let file = std::fs::File::open(proc_dir.join("maps"))?;
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
    mod tests {
        use super::*;

        #[test]
        fn nested_roots_keep_a_witness_each_consumer_can_place() {
            let dirs = [PathBuf::from("/cache"), PathBuf::from("/cache/sub\nline")];
            let row = b"1000-2000 r--p 0000 01:02 7 /cache/sub\\012line/file\n";
            let got = parse(row, 7, &dirs).unwrap().expect("mapped file");
            assert!(
                dirs.iter()
                    .all(|root| placed(&got.path, std::slice::from_ref(root)).is_some()),
                "{got:?}"
            );
        }

        #[test]
        fn ambiguous_known_roots_are_unknown_instead_of_selecting_one() {
            let dirs = [
                PathBuf::from("/cache\\012line"),
                PathBuf::from("/cache\nline"),
            ];
            let row = b"1000-2000 r--p 0000 01:02 7 /cache\\012line/file\n";
            assert!(
                parse(row, 7, &dirs).is_err(),
                "neither possible owner may be ruled out"
            );
            for root in &dirs {
                let got = parse(row, 7, std::slice::from_ref(root))
                    .unwrap()
                    .expect("possible holder");
                assert!(placed(&got.path, std::slice::from_ref(root)).is_some());
            }
        }

        #[test]
        fn maps_keep_raw_paths_spaces_newlines_and_deleted_names() {
            for name in [
                b"/cache/input with spaces".as_slice(),
                b"/cache/byte-\xff",
                b"/cache/input (deleted)",
            ] {
                let mut row = b"1000-2000 r--p 0000 01:02 7 ".to_vec();
                row.extend_from_slice(name);
                row.push(b'\n');
                let got = parse(&row, 7, &[PathBuf::from("/cache")])
                    .expect("complete row")
                    .expect("mapped file");
                assert_eq!(got.how, "map");
                assert_eq!(got.path.as_os_str().as_bytes(), name);
            }
            let row = b"1000-2000 r--p 0000 01:02 7 /cache\\012line/file\n";
            for root in ["/cache\nline", "/cache\\012line"] {
                assert!(
                    parse(row, 7, &[PathBuf::from(root)]).unwrap().is_some(),
                    "{root:?}"
                );
            }
            let root = PathBuf::from("/cache\nline\\012literal");
            for prefix in ["", "/System/Volumes/Data"] {
                let row = format!(
                    "1000-2000 r--p 0000 01:02 7 {prefix}/cache\\012line\\012literal/file\n"
                );
                let got = parse(row.as_bytes(), 7, std::slice::from_ref(&root))
                    .unwrap()
                    .expect("mixed newline and literal escape preserve a possible holder");
                assert!(
                    got.path.starts_with(&root),
                    "the consumer can place the witness: {got:?}"
                );
            }
        }

        #[test]
        fn incomplete_malformed_and_unreadable_maps_are_unknown() {
            for row in [
                b"1000-2000 r--p 0000 01:02 7 /other\ntruncated".as_slice(),
                b"1000-2000 r--p 0000 01:02 7 /other",
                b"1000-2000 r--p 0000 01:02 7 /other\0name\n",
                b"1000-2000 r--p 0000 01:02 7\n",
                b"2000-1000 r--p 0000 01:02 7 /other\n",
                b"1000-2000 ---- 0000 01:02 0\n",
                b"1000-2000 r--p 0000 01:02 0\n1000-2000 r--p 0000 01:02 0\n",
            ] {
                assert!(
                    parse(row, 7, &[PathBuf::from("/cache")]).is_err(),
                    "{row:?}"
                );
            }
            let mut oversized = b"1000-2000 r--p 0000 01:02 7 /other/".to_vec();
            oversized.extend(std::iter::repeat_n(b'x', MAX_BYTES));
            oversized.push(b'\n');
            assert!(
                parse(&oversized, 7, &[]).is_err(),
                "otherwise valid data still has a bound"
            );
            let world = aterm_tempfile::tempdir().unwrap();
            assert!(
                uses(world.path(), 7, &[]).is_err(),
                "missing maps of an existing process is unknown"
            );
            std::fs::create_dir(world.path().join("maps")).unwrap();
            assert!(
                uses(world.path(), 7, &[]).is_err(),
                "a failed read is unknown"
            );
            assert!(
                uses(&world.path().join("exited"), 7, &[])
                    .unwrap()
                    .is_none()
            );
            assert!(
                parse(
                b"1000-2000 rw-p 0000 00:00 0 [heap]\n2000-3000 r--p 0000 01:02 1 /cache-neighbor\n",
                    7,
                    &[PathBuf::from("/cache")]
                )
                .unwrap()
                .is_none()
            );
        }
    }
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
mod mac_tests {
    use super::*;

    #[test]
    fn short_truncated_and_unnamed_file_regions_are_unknown() {
        let mut buf = [0u8; ffi::REGION_SIZE];
        buf[ffi::REGION_ADDRESS..ffi::REGION_ADDRESS + 8].copy_from_slice(&4096u64.to_ne_bytes());
        buf[ffi::REGION_LEN..ffi::REGION_LEN + 8].copy_from_slice(&4096u64.to_ne_bytes());
        assert!(
            mac_region(&buf).unwrap().path.is_none(),
            "an anonymous region"
        );
        assert!(mac_region(&buf[..buf.len() - 1]).is_err());
        buf[ffi::REGION_INODE..ffi::REGION_INODE + 8].copy_from_slice(&7u64.to_ne_bytes());
        assert!(
            mac_region(&buf).is_err(),
            "a vnode without its path is unknown"
        );
        buf[ffi::REGION_PATH..].fill(b'x');
        assert!(mac_region(&buf).is_err(), "an unterminated path is unknown");
        buf[ffi::REGION_SIZE - 1] = 0;
        assert!(
            mac_region(&buf).is_err(),
            "the forced final terminator may hide truncation"
        );
        buf[ffi::REGION_PATH..].fill(0);
        buf[ffi::REGION_PATH..ffi::REGION_PATH + 11].copy_from_slice(b"/cache/file");
        assert_eq!(
            mac_region(&buf).unwrap().path,
            Some(PathBuf::from("/cache/file"))
        );
    }

    #[test]
    fn refused_exited_nonadvancing_overflowing_and_bounded_walks_keep_their_meaning() {
        let dirs = [PathBuf::from("/cache")];
        for failure in [Failed::Refused, Failed::Other, Failed::Exited] {
            let got = mac_with(7, &dirs, |at| {
                if at == 0 {
                    Ok(Some(Region {
                        address: 0,
                        len: 1,
                        path: None,
                    }))
                } else {
                    Err(failure)
                }
            });
            assert!(matches!(got, Err(f) if f == failure));
        }
        assert!(
            mac_with(7, &dirs, |_| Ok(Some(Region {
                address: 0,
                len: 1,
                path: None
            })))
            .is_err()
        );
        assert!(
            mac_with(7, &dirs, |_| Ok(Some(Region {
                address: u64::MAX,
                len: 1,
                path: None
            })))
            .is_err()
        );
        assert!(
            mac_with(7, &dirs, |at| Ok(Some(Region {
                address: at,
                len: 1,
                path: None
            })))
            .is_err()
        );
        assert!(mac_with(7, &dirs, |_| Ok(None)).unwrap().is_none());
        let got = mac_with(7, &dirs, |_| {
            Ok(Some(Region {
                address: 0,
                len: 1,
                path: Some(PathBuf::from("/cache/file")),
            }))
        })
        .unwrap()
        .expect("a mapping is already a holder");
        assert_eq!(got.how, "map");
    }

    #[test]
    fn only_the_kernel_end_of_regions_is_a_complete_walk() {
        assert!(mac_answer(0, libc::EINVAL, &[]).unwrap().is_none());
        for error in [0, libc::EACCES, libc::EPERM, libc::EIO, libc::ESRCH] {
            assert!(matches!(mac_answer(0, error, &[]), Err(f) if f == Failed::of(error)));
        }
        assert!(mac_answer((ffi::REGION_SIZE - 1) as i32, 0, &[0; ffi::REGION_SIZE]).is_err());
    }
}
