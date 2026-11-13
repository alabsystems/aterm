// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn scratch(name: &str) -> crate::testdir::Dir {
    crate::testdir::dir(&format!("atpkg-keys-fsio-{name}-"))
}

/// The ordinary case: the file is replaced, and no temporary is left beside it. Both
/// the staged bytes and the renamed entry are made durable.
#[test]
fn an_atomic_write_replaces_the_file_and_leaves_nothing_behind() {
    let dir = scratch("replace");
    let path = dir.join("pins.rs").to_str().unwrap().to_string();
    std::fs::write(&path, b"old").unwrap();

    let recording = aterm_durable::seam::Recording::start();
    write_bytes_atomic(&path, b"new content").expect("the write succeeds");
    assert_eq!(recording.take(), [aterm_durable::Flush::Durable; 2]);
    assert_eq!(std::fs::read(&path).unwrap(), b"new content");
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        leftovers,
        vec!["pins.rs".to_string()],
        "no temporary survives"
    );

    let fresh = dir.join("new.rs").to_str().unwrap().to_string();
    write_bytes_atomic(&fresh, b"hello").unwrap();
    assert_eq!(std::fs::read(&fresh).unwrap(), b"hello");

    let link = dir.join("sig-link.sig");
    std::os::unix::fs::symlink(&fresh, &link).unwrap();
    let _ = recording.take();
    write_bytes(link.to_str().unwrap(), b"detached-sig").unwrap();
    assert_eq!(recording.take(), [aterm_durable::Flush::Durable; 2]);
    assert!(
        !std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "write_bytes replaces a symlink rather than writing through it"
    );
    assert_eq!(std::fs::read(&fresh).unwrap(), b"hello");
    assert_eq!(std::fs::read(&link).unwrap(), b"detached-sig");
}

/// A parent obstructed before staging fails without changing the original, even as root.
#[test]
fn a_failed_atomic_write_leaves_the_original_intact() {
    let root = scratch("failed");
    let dir = root.join("anchor");
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("pins.rs").to_str().unwrap().to_string();
    std::fs::write(&path, b"the trust anchor").unwrap();

    let saved = root.join("anchor-before-staging");
    let (move_from, move_to) = (dir.clone(), saved.clone());
    let result = window::with(
        "temp name drawn",
        move |_| {
            std::fs::rename(&move_from, &move_to).unwrap();
            std::fs::write(&move_from, b"a parent must be a directory").unwrap();
        },
        || write_bytes_atomic(&path, b"a replacement that cannot land"),
    );
    std::fs::remove_file(&dir).unwrap();
    std::fs::rename(&saved, &dir).unwrap();

    assert!(result.is_err(), "the write must fail");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"the trust anchor",
        "the original is untouched — not truncated, not partial"
    );
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(leftovers, vec!["pins.rs".to_string()], "no litter");

    let squatted = dir.join("roster.toml");
    std::fs::create_dir(&squatted).unwrap();
    assert!(write_bytes_atomic(squatted.to_str().unwrap(), b"body").is_err());
    let mut leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    leftovers.sort();
    assert_eq!(leftovers, ["pins.rs", "roster.toml"], "no litter");
}

/// A read is bounded at the cap, not one byte short of it, and refuses a file that is
/// not regular before reading a byte.
#[test]
fn a_read_is_bounded_at_its_cap_and_refuses_non_regular_files() {
    let dir = scratch("read-cap");
    let path = dir.join("doc").to_str().unwrap().to_string();
    std::fs::write(&path, b"12345678").unwrap();
    assert_eq!(read_bytes_up_to(&path, 8).unwrap(), b"12345678");
    std::fs::write(&path, b"123456789").unwrap();
    let err = read_bytes_up_to(&path, 8).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("8-byte read cap"), "{err}");
    assert_eq!(read_bytes_up_to(&path, u64::MAX).unwrap(), b"123456789");
    std::fs::write(&path, vec![b'x'; (READ_CAP as usize) + 1]).unwrap();
    let mib_err = read_bytes(&path).unwrap_err();
    assert!(mib_err.to_string().contains("1 MiB read cap"), "{mib_err}");
    assert!(read_bytes(dir.to_str().unwrap()).is_err(), "a directory");
    let fifo = dir.join("pipe");
    let fifo_str = fifo.to_str().unwrap().to_string();
    let c_fifo = std::ffi::CString::new(fifo_str.as_str()).unwrap();
    // SAFETY: `c_fifo` is a valid NUL-terminated path owned across `mkfifo`.
    assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
    let fifo_err = read_bytes(&fifo_str).unwrap_err();
    assert!(
        fifo_err
            .to_string()
            .contains("is a FIFO, not a regular file"),
        "{fifo_err}"
    );
}

/// Mutation check: with a fixed temporary name the write below fails with EEXIST.
#[test]
fn a_crash_left_temp_never_wedges_the_next_atomic_write() {
    let dir = scratch("stale-temp");
    let path = dir.join("roster.toml").to_str().unwrap().to_string();
    let stale = concat(&[&path, ".atpkg-keys.tmp"]);
    std::fs::write(&stale, b"bytes from a dead process").unwrap();

    write_bytes_atomic(&path, b"current").expect("a unique temp bypasses stale litter");
    assert_eq!(std::fs::read(&path).unwrap(), b"current");
    assert_eq!(std::fs::read(&stale).unwrap(), b"bytes from a dead process");

    let corpse = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
    let found = std::rc::Rc::clone(&corpse);
    window::with(
        "temp name drawn",
        move |tmp| {
            std::fs::write(tmp, b"a reused pid's corpse").unwrap();
            *found.borrow_mut() = tmp.to_string();
        },
        || write_bytes_atomic(&path, b"later").expect("the next name is free"),
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"later");
    assert_eq!(
        std::fs::read(corpse.borrow().as_str()).unwrap(),
        b"a reused pid's corpse"
    );
}

/// `create_new` + `write_all` would expose a zero-length key under its final name and
/// `write_bytes_atomic` would clobber; link-after-fsync is whole, private and never replaces.
#[test]
fn an_owner_file_is_complete_private_and_never_replaced() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = scratch("owner-create-new");
    let path = dir.join("machine.toml").to_str().unwrap().to_string();

    let recording = aterm_durable::seam::Recording::start();
    write_owner_file_create_new(&path, b"first").unwrap();
    assert_eq!(recording.take(), [aterm_durable::Flush::Durable; 3]);
    assert_eq!(std::fs::read(&path).unwrap(), b"first");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );

    assert!(write_owner_file_create_new(&path, b"second").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"first");
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        leftovers,
        vec!["machine.toml".to_string()],
        "the refused write leaves no staged temporary behind"
    );
}

#[test]
fn ensure_parent_dir_creates_a_missing_chain_and_is_idempotent() {
    let dir = scratch("parents");
    let key = dir
        .join("home/.aterm/machine.key")
        .to_str()
        .unwrap()
        .to_string();
    assert!(!dir.join("home/.aterm").exists());
    let recording = aterm_durable::seam::Recording::start();
    ensure_parent_dir(&key).expect("creates the chain");
    assert!(dir.join("home/.aterm").is_dir());
    assert_eq!(recording.take(), [aterm_durable::Flush::Durable; 2]);
    ensure_parent_dir(&key).expect("a second call is a no-op");
    assert_eq!(recording.take(), []);
    ensure_parent_dir("machine.key").expect("no parent, no problem");
    sync_parent("machine.key").expect("a bare name syncs the current directory");
}
