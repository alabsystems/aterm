// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_clone_is_a_new_file_with_the_same_bytes_and_mode() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let dir = aterm_tempfile::tempdir().unwrap();
    let src = dir.path().join("base.img");
    std::fs::write(&src, b"ext4 or so").unwrap();
    std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o444)).unwrap();
    let dst = dir.path().join("clone.img");
    clone_file(&src, &dst).unwrap();
    let (a, b) = (
        std::fs::metadata(&src).unwrap(),
        std::fs::metadata(&dst).unwrap(),
    );
    assert_ne!(a.ino(), b.ino(), "a clone is its own inode");
    assert_eq!(std::fs::read(&dst).unwrap(), b"ext4 or so");
    assert_eq!(
        b.permissions().mode() & 0o777,
        0o444,
        "and carries its mode"
    );

    let again = clone_file(&src, &dst).unwrap_err();
    assert_eq!(again.kind(), io::ErrorKind::AlreadyExists, "{again}");
    let link = dir.path().join("link.img");
    std::os::unix::fs::symlink(&src, &link).unwrap();
    assert_eq!(
        clone_file(&link, &dir.path().join("through.img"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput,
        "a link is never cloned through"
    );
}

/// Where on its device the byte at `offset` of `file` lies (`fcntl(F_LOG2PHYS_EXT)`).
fn device_offset(file: &std::fs::File, offset: i64) -> i64 {
    use std::os::fd::AsRawFd as _;

    /// `<sys/fcntl.h>`: map a file offset to a device offset.
    const F_LOG2PHYS_EXT: i32 = 65;

    /// `<sys/fcntl.h>`'s `struct log2phys`, under its `#pragma pack(4)`.
    #[repr(C, packed(4))]
    struct Log2Phys {
        flags: u32,
        contigbytes: i64,
        devoffset: i64,
    }

    unsafe extern "C" {
        /// `<fcntl.h>`: `int fcntl(int fildes, int cmd, ...)`.
        fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    }

    let mut l2p = Log2Phys {
        flags: 0,
        contigbytes: 4096,
        devoffset: offset,
    };
    // SAFETY: the descriptor is open for the call, and `l2p` is the struct the command
    // reads and writes, alive and exclusively borrowed across it.
    let rc = unsafe { fcntl(file.as_raw_fd(), F_LOG2PHYS_EXT, &raw mut l2p) };
    assert_eq!(rc, 0, "F_LOG2PHYS_EXT: {}", io::Error::last_os_error());
    l2p.devoffset
}

/// The negative control is a byte copy of the same file.
#[test]
fn a_clone_shares_its_sources_blocks_and_a_copy_does_not() {
    use std::io::Write as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let src = dir.path().join("base.img");
    let mut file = std::fs::File::create(&src).unwrap();
    file.write_all(&vec![0x5au8; 1 << 20]).unwrap();
    aterm_durable::durable(&file).unwrap();
    drop(file);
    let dst = dir.path().join("clone.img");
    clone_file(&src, &dst).unwrap();
    let at = |path: &Path| device_offset(&std::fs::File::open(path).unwrap(), 0);
    assert_eq!(
        at(&dst),
        at(&src),
        "the clone's first block is the source's"
    );

    let copy = dir.path().join("copy.img");
    let mut out = std::fs::File::create_new(&copy).unwrap();
    out.write_all(&std::fs::read(&src).unwrap()).unwrap();
    aterm_durable::durable(&out).unwrap();
    drop(out);
    assert_ne!(at(&copy), at(&src), "a byte copy has blocks of its own");
}

#[test]
fn a_clone_grown_to_the_disk_size_stays_sparse() {
    use std::os::unix::fs::MetadataExt as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let src = dir.path().join("base.img");
    std::fs::write(&src, vec![7u8; 1 << 20]).unwrap();
    let dst = dir.path().join("disk.img");
    clone_file(&src, &dst).unwrap();
    let grown: u64 = 8 << 30;
    std::fs::File::options()
        .write(true)
        .open(&dst)
        .unwrap()
        .set_len(grown)
        .unwrap();
    let meta = std::fs::metadata(&dst).unwrap();
    assert_eq!(meta.len(), grown);
    assert!(
        meta.blocks() * 512 < 64 << 20,
        "an 8 GiB disk grown from a 1 MiB clone allocates {} bytes",
        meta.blocks() * 512
    );
    assert_eq!(
        std::fs::read(&src).unwrap().len(),
        1 << 20,
        "the source is untouched"
    );
}
