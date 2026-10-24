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
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The key the disk (or device) at `path` holds: `Ok(None)` where it is not a key disk.
///
/// # Errors
/// It could not be opened or read.
pub fn read_disk(path: &Path) -> io::Result<Option<Key>> {
    let mut f = std::fs::File::open(path)?;
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
    names.sort();
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
mod tests {
    use super::*;

    #[test]
    fn a_sector_round_trips_and_anything_else_is_no_key() {
        let key = [0x5au8; LEN];
        let s = sector(&key);
        assert_eq!(&s[..16], b"aterm-vz-key v1\n");
        assert_eq!(parse(&s), Some(key));
        assert!(s[16 + LEN..].iter().all(|b| *b == 0));
        // An ext4 root's first sector (zeros), an ISO's, a short read, another magic.
        assert_eq!(parse(&[0u8; SECTOR]), None);
        assert_eq!(parse(&s[..16 + LEN - 1]), None);
        let mut other = s;
        other[0] = b'A';
        assert_eq!(parse(&other), None);
    }

    #[test]
    fn keys_compare_whole() {
        let a = [7u8; LEN];
        let mut b = a;
        assert!(same(&a, &b));
        b[LEN - 1] ^= 1;
        assert!(!same(&a, &b));
        b = a;
        b[0] ^= 0x80;
        assert!(!same(&a, &b));
    }

    #[test]
    fn the_guest_finds_the_key_disk_among_its_virtio_disks_by_its_magic() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let sys = dir.path().join("sys");
        let dev = dir.path().join("dev");
        std::fs::create_dir_all(&sys).unwrap();
        std::fs::create_dir_all(&dev).unwrap();
        assert!(find(&sys, &dev).unwrap_err().contains("no virtio disk"));
        // vda the root (no key), vdb the bake's seed (no key), vdc the key disk; loop0 and a
        // name that is no virtio disk are never read.
        for name in ["vda", "vdb", "vdc", "loop0"] {
            std::fs::create_dir(sys.join(name)).unwrap();
        }
        std::fs::write(dev.join("vda"), [0u8; 4096]).unwrap();
        std::fs::write(dev.join("vdb"), [1u8; 100]).unwrap();
        let key = [0xc3u8; LEN];
        let mut disk = sector(&key).to_vec();
        disk.resize(4096, 0);
        std::fs::write(dev.join("vdc"), &disk).unwrap();
        std::fs::write(dev.join("loop0"), sector(&[9u8; LEN])).unwrap();
        assert_eq!(find(&sys, &dev), Ok(key));
        // The negative control: without the key disk, it names what it read.
        std::fs::remove_file(dev.join("vdc")).unwrap();
        let why = find(&sys, &dev).unwrap_err();
        assert!(
            why.contains("vda: no key") && why.contains("vdb: no key") && why.contains("vdc: "),
            "{why}"
        );
        assert!(!why.contains("loop0"), "{why}");
    }
}
