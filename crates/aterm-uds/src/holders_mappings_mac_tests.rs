// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn actual_native_mapping_walk_survives_a_large_unrelated_prefix() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let pid = std::process::id();
    let native = i32::try_from(pid).unwrap();
    let mut calls = 0;
    let found = mac_with(pid, &[scratch.path().to_owned()], |at| {
        calls += 1;
        if calls <= 65_536 {
            Ok(Some(Region {
                address: at,
                len: 1,
                path: None,
            }))
        } else {
            read_mac_region(native, at)
        }
    })
    .unwrap();
    assert!(found.is_none());
    assert!(calls > 65_536);
    let mut called = false;
    assert!(
        mac_started(
            pid,
            &[scratch.path().to_owned()],
            |_| {
                called = true;
                Ok(None)
            },
            std::time::Instant::now() - std::time::Duration::from_secs(61)
        )
        .is_err()
    );
    assert!(!called);
}

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
        mac_started(
            7,
            &dirs,
            |at| Ok(Some(Region {
                address: at,
                len: 1,
                path: None
            })),
            std::time::Instant::now() - std::time::Duration::from_secs(61)
        )
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
