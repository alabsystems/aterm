// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The boot's key (`docs/DESIGN-vz-guest-2026-10-23.md` §6): how the helper tells the agent's
//! watch dial from any other guest process's.
//!
//! Any guest process may dial the host's vsock port 1025, and one that dials before the agent
//! would otherwise be the watch connection, free to forge its `Hello`, `Ready` and `Beat`s. So
//! at each `Boot` the helper makes a fresh 32-byte secret and writes it, as one sector, to a
//! small raw disk it attaches read-only: the key disk. In the guest a block device is
//! `root:disk 0660` (udev's default; devtmpfs makes it `root 0600` before udev runs), and the
//! lane user is in no `disk` group, so only guest root reads it. It is never on the kernel
//! command line, which `/proc/cmdline` shows every user. The agent, root, reads it at its
//! start and sends it in its `Hello`; the helper believes no frame of a dial whose first frame
//! is not a `Hello` carrying this boot's key, drops that dial, and keeps listening.
//!
//! The sector is [`MAGIC`], then the key, then zeros; the disk is [`DISK_BYTES`], sparse.

use std::io::{self, Read as _};
use std::path::Path;

/// The key disk's first bytes.
pub const MAGIC: [u8; 16] = *b"aterm-vz-key v1\n";

/// A key's length.
pub const LEN: usize = 32;

/// One boot's key.
pub type Key = [u8; LEN];

/// The sector the key is written in.
pub const SECTOR: usize = 512;

/// The key disk's size: 1 MiB, sparse (a disk image is whole sectors).
pub const DISK_BYTES: u64 = 1 << 20;

/// The key disk's first sector for `key`.
#[must_use]
pub fn sector(key: &Key) -> [u8; SECTOR] {
    let mut out = [0u8; SECTOR];
    out[..MAGIC.len()].copy_from_slice(&MAGIC);
    out[MAGIC.len()..MAGIC.len() + LEN].copy_from_slice(key);
    out
}

/// The key a key disk's first bytes hold, or `None` where they are not a key disk's.
#[must_use]
pub fn parse(bytes: &[u8]) -> Option<Key> {
    let rest = bytes.strip_prefix(&MAGIC)?;
    rest.first_chunk::<LEN>().copied()
}

/// Whether two keys are the same, in time that does not depend on where they differ.
#[must_use]
pub fn same(a: &Key, b: &Key) -> bool {
    aterm_digest::ct_eq(a, b)
}

/// The key the disk (or device) at `path` holds: `Ok(None)` where it is not a key disk.
///
/// # Errors
/// It could not be opened or read.
pub fn read_disk(path: &Path) -> io::Result<Option<Key>> {
    let mut f = crate::open_nonblock(path)?;
    let mut buf = [0u8; SECTOR];
    let mut got = 0;
    while got < buf.len() {
        match f.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(parse(&buf[..got]))
}

/// The boot's key in the guest: the first virtio disk (`vd*` under `sys_block`, in name order)
/// whose device under `dev` is a key disk. The agent reads `/sys/block` and `/dev`.
///
/// # Errors
/// No disk is a key disk: what was read, so the console says why the host will refuse the
/// agent's hello.
pub fn find(sys_block: &Path, dev: &Path) -> Result<Key, String> {
    let mut names: Vec<String> = std::fs::read_dir(sys_block)
        .map_err(|e| format!("{}: {e}", sys_block.display()))?
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| name.starts_with("vd"))
        .collect();
    names.sort_unstable();
    let mut read = Vec::new();
    for name in &names {
        let device = dev.join(name);
        match read_disk(&device) {
            Ok(Some(key)) => return Ok(key),
            Ok(None) => read.push(format!("{name}: no key")),
            Err(e) => read.push(format!("{name}: {e}")),
        }
    }
    Err(if read.is_empty() {
        format!("no virtio disk under {}", sys_block.display())
    } else {
        format!("no key disk ({})", read.join("; "))
    })
}

#[cfg(test)]
#[path = "key_tests.rs"]
mod tests;
