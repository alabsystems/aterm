// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
