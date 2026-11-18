// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! THE WINDOWS HALF OF A TRACE'S LOOK ([`super::Look`]): what a name's handle shows beyond std's
//! `Metadata`, which on Windows holds no volume, file id, link count, change time, owner or
//! access list, the facts this platform's readers decide a file by (`private_file`'s owner-only
//! rule and second-link refusal, `aterm-secrets`' admitted reads). Without them a change of who
//! may use a file, or of its names, would move no stamp, and a read refused (or admitted) by
//! them would stand past the change.

use std::fs::File;
use std::io;
use std::os::windows::fs::OpenOptionsExt as _;
use std::path::Path;

use aterm_dirfd::acl::{self, Descriptor, Parts};

/// `READ_CONTROL`: read the object's descriptor.
const READ_CONTROL: u32 = 0x0002_0000;
/// `FILE_READ_ATTRIBUTES`.
const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
/// `SYNCHRONIZE`.
const SYNCHRONIZE: u32 = 0x0010_0000;
/// `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`: the look holds nobody off the file.
const SHARE_ALL: u32 = 0x0000_0007;
/// `ERROR_ACCESS_DENIED`.
const ERROR_ACCESS_DENIED: i32 = 5;
/// `ERROR_INVALID_PARAMETER`: what a `FileIdInfo` query answers on a file system that has no file
/// ids (FAT, exFAT).
const ERROR_INVALID_PARAMETER: i32 = 87;
/// The FILETIME tick (100 ns since 1601) of the Unix epoch.
const FILETIME_UNIX_EPOCH: i128 = 116_444_736_000_000_000;

/// What a name's handle shows beyond std's `Metadata` ([`super::Meta`] on Windows).
#[derive(Clone, Copy, Debug)]
pub(super) struct Facts {
    /// The volume's serial number; 0 on a file system with no file ids.
    pub(super) dev: u64,
    /// The 128-bit file id, its two halves XORed; 0 on a file system with no file ids.
    pub(super) ino: u64,
    /// Its names (`NumberOfLinks`).
    pub(super) nlink: u64,
    /// Its change time (Unix nanoseconds), which a write, an attribute change and an access-list
    /// change move; `None` on a file system with no file ids.
    pub(super) changed_ns: Option<i128>,
    /// Its attributes (`FILE_ATTRIBUTE_*`).
    pub(super) attributes: u32,
    /// The first four bytes of the SHA-256 of its owner and access list (their SDDL text); 0
    /// where this user may not read its descriptor.
    pub(super) access: u32,
}

impl Facts {
    /// What `path` shows now, opened as itself (a reparse point opened, never followed) or, with
    /// `follow`, through a link to what it names; to read its attributes and, where this user
    /// may, its descriptor (`READ_CONTROL`), sharing everything.
    ///
    /// # Errors
    /// The open's, the descriptor's or a query's error.
    pub(super) fn of(path: &Path, follow: bool) -> io::Result<Self> {
        let reparse = if follow {
            0
        } else {
            acl::FILE_FLAG_OPEN_REPARSE_POINT
        };
        let open = |access: u32| {
            std::fs::OpenOptions::new()
                .access_mode(access | FILE_READ_ATTRIBUTES | SYNCHRONIZE)
                .share_mode(SHARE_ALL)
                .custom_flags(acl::FILE_FLAG_BACKUP_SEMANTICS | reparse)
                .open(path)
        };
        let (file, access) = match open(READ_CONTROL) {
            Ok(file) => {
                let access = access_of(&file)?;
                (file, access)
            }
            Err(error) if error.raw_os_error() == Some(ERROR_ACCESS_DENIED) => (open(0)?, 0),
            Err(error) => return Err(error),
        };
        match aterm_dirfd::fstat(&file) {
            Ok(stat) => Ok(Self {
                dev: stat.st_dev,
                ino: fold(stat.st_ino),
                nlink: stat.st_nlink,
                changed_ns: Some((i128::from(stat.change_time) - FILETIME_UNIX_EPOCH) * 100),
                attributes: stat.attributes,
                access,
            }),
            // No file ids: FAT or exFAT, which keeps no access list or second name either.
            Err(error) if error.raw_os_error() == ERROR_INVALID_PARAMETER => {
                let facts = acl::by_handle(&file)?;
                Ok(Self {
                    dev: 0,
                    ino: 0,
                    nlink: u64::from(facts.links),
                    changed_ns: None,
                    attributes: facts.attributes,
                    access,
                })
            }
            Err(error) => Err(io::Error::from(error)),
        }
    }
}

/// A 128-bit file id in 64 bits: its halves XORed, so an id ReFS fills past 64 bits still moves
/// what a stamp holds.
const fn fold(id: u128) -> u64 {
    (id as u64) ^ ((id >> 64) as u64)
}

/// The first four bytes of the SHA-256 of the owner and access list of what `file` is open on.
///
/// # Errors
/// The descriptor could not be read or written as text.
fn access_of(file: &File) -> io::Result<u32> {
    let parts = Parts::OWNER | Parts::DACL;
    let text = Descriptor::of(file, parts)?.to_sddl(parts)?;
    let mut digest = aterm_digest::Sha256::new();
    digest.update(b"aterm-uds/trace/access/v1\0");
    digest.update(text.as_bytes());
    let digest = digest.finalize();
    Ok(u32::from_le_bytes([
        digest[0], digest[1], digest[2], digest[3],
    ]))
}
