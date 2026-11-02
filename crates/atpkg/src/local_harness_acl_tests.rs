// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native ACLs on private data fixtures; no utility, executable copy or process is started.

use super::*;
use std::ffi::{CStr, c_void};
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd as _;

// Signatures and constants are from the macOS SDK's sys/acl.h. ACL handles are freed once.
unsafe extern "C" {
    fn acl_init(count: i32) -> *mut c_void;
    fn acl_create_entry(acl: *mut *mut c_void, entry: *mut *mut c_void) -> i32;
    fn acl_set_tag_type(entry: *mut c_void, tag: i32) -> i32;
    fn acl_set_qualifier(entry: *mut c_void, qualifier: *const c_void) -> i32;
    fn acl_set_permset_mask_np(entry: *mut c_void, mask: u64) -> i32;
    fn acl_set_fd_np(fd: i32, acl: *mut c_void, kind: i32) -> i32;
    fn acl_get_fd_np(fd: i32, kind: i32) -> *mut c_void;
    fn acl_to_text(acl: *mut c_void, len: *mut isize) -> *mut i8;
    fn acl_free(object: *mut c_void) -> i32;
}
struct Acl(*mut c_void);
impl Drop for Acl {
    fn drop(&mut self) {
        // SAFETY: this is the sole owner of an ACL or text allocation from libSystem.
        if !self.0.is_null() {
            unsafe { acl_free(self.0) };
        }
    }
}
fn allow_other_write(file: &File) {
    // membership.h documents this synthesized UUID prefix for a numeric user ID.
    // The distinct UID needs no actual account, sign-in or membership lookup.
    let mut uuid = [
        0xff, 0xff, 0xee, 0xee, 0xdd, 0xdd, 0xcc, 0xcc, 0xbb, 0xbb, 0xaa, 0xaa, 0, 0, 0, 0,
    ];
    uuid[12..].copy_from_slice(&crate::platform::our_uid().wrapping_add(1).to_be_bytes());
    // SAFETY: the returned allocation is owned here; each output pointer is writable and
    // initialized before use, and the UUID's 16 bytes outlive the copying qualifier call.
    unsafe {
        let mut acl = Acl(acl_init(1));
        assert!(!acl.0.is_null(), "acl_init: {}", io::Error::last_os_error());
        let mut entry = std::ptr::null_mut();
        assert_eq!(acl_create_entry(&raw mut acl.0, &raw mut entry), 0);
        assert_eq!(acl_set_tag_type(entry, 1), 0); // ACL_EXTENDED_ALLOW.
        assert_eq!(acl_set_qualifier(entry, uuid.as_ptr().cast()), 0);
        assert_eq!(acl_set_permset_mask_np(entry, 1 << 2), 0); // WRITE_DATA / ADD_FILE.
        assert_eq!(acl_set_fd_np(file.as_raw_fd(), acl.0, 0x100), 0);
    }
}
fn acl_text(file: &File) -> Vec<u8> {
    // SAFETY: file stays open; successful ACL/text calls return owned allocations. The SDK
    // guarantees NUL-terminated text, copied before its allocation and the ACL are freed.
    unsafe {
        let acl = Acl(acl_get_fd_np(file.as_raw_fd(), 0x100));
        assert!(!acl.0.is_null());
        let mut len = 0;
        let text = acl_to_text(acl.0, &raw mut len);
        assert!(!text.is_null());
        let _text = Acl(text.cast());
        assert!((0..65536).contains(&len));
        CStr::from_ptr(text).to_bytes().to_vec()
    }
}
fn clear_acl(file: &File) {
    // SAFETY: the empty owned ACL and retained fixture descriptor are valid through set_fd.
    unsafe {
        let acl = Acl(acl_init(0));
        assert!(!acl.0.is_null());
        assert_eq!(acl_set_fd_np(file.as_raw_fd(), acl.0, 0x100), 0);
    }
}
fn witness(file: &File) -> (u32, Vec<u8>) {
    (file.metadata().unwrap().mode(), acl_text(file))
}

#[test]
fn allow_acls_on_0700_and_0755_input_or_store_custody_refuse_without_changes() {
    for mode in [0o700, 0o755] {
        for at in [
            "artifact",
            "input-bin",
            "executable",
            "receipt",
            "store-program",
            "store-lock",
        ] {
            let w = World::new("local-harness-input-allow-acl");
            let lock = w.lock();
            let _parent = fs::Parent::open(&lock, PROGRAM, true).unwrap();
            let binary = w.artifact.join("bin").join(COMMAND);
            let receipt = w.artifact.join("local-build.json");
            let path = match at {
                "artifact" => w.artifact.clone(),
                "input-bin" => w.artifact.join("bin"),
                "executable" => binary.clone(),
                "receipt" => receipt.clone(),
                "store-program" => w.build().parent().unwrap().to_path_buf(),
                _ => w.layout.store_lock(),
            };
            if path.is_dir() {
                crate::platform::set_mode(&path, mode).unwrap();
            }
            let file = File::open(&path).unwrap();
            let payload = std::fs::read(&binary).unwrap();
            let metadata = std::fs::read(&receipt).unwrap();
            let outside = w.artifact.parent().unwrap().join("outside");
            std::fs::write(&outside, b"untouched").unwrap();
            allow_other_write(&file);
            let before = witness(&file);
            assert!(
                stage(&lock, &w.authority(), &w.artifact).is_err(),
                "{at} {mode:o}"
            );
            assert_eq!(witness(&file), before, "{at}: ACL and mode left untouched");
            assert_eq!(std::fs::read(binary).unwrap(), payload);
            assert_eq!(std::fs::read(receipt).unwrap(), metadata);
            assert_eq!(std::fs::read(outside).unwrap(), b"untouched");
            assert!(std::fs::symlink_metadata(w.build()).is_err());
            clear_acl(&file); // Owner's fixture cleanup follows all refusal/evidence assertions.
        }
    }
}

#[test]
fn allow_acls_on_complete_payload_or_metadata_cannot_select_current() {
    for at in ["root", "bin", "executable", "record", "ready", "store-lock"] {
        let w = World::new("local-harness-selected-allow-acl");
        let lock = w.lock();
        let staged = stage(&lock, &w.authority(), &w.artifact).unwrap();
        let path = match at {
            "root" => w.build(),
            "bin" => w.build().join("bin"),
            "executable" => w.build().join("bin").join(COMMAND),
            "record" => crate::store::sidecar_path(&w.build(), SUFFIX).unwrap(),
            "ready" => crate::store::sidecar_path(&w.build(), ".ready").unwrap(),
            _ => w.layout.store_lock(),
        };
        let current = w.layout.program_current(PROGRAM);
        let old = w.layout.build_dir(PROGRAM, 41);
        std::os::unix::fs::symlink(&old, &current).unwrap();
        let file = File::open(path).unwrap();
        let payload = std::fs::read(w.build().join("bin").join(COMMAND)).unwrap();
        let record =
            std::fs::read(crate::store::sidecar_path(&w.build(), SUFFIX).unwrap()).unwrap();
        allow_other_write(&file);
        let before = witness(&file);
        assert!(
            select_current(&lock, &staged, ExpectedCurrent::Build(41)).is_err(),
            "{at}"
        );
        assert_eq!(witness(&file), before);
        assert_eq!(std::fs::read_link(current).unwrap(), old);
        assert_eq!(
            std::fs::read(w.build().join("bin").join(COMMAND)).unwrap(),
            payload
        );
        assert_eq!(
            std::fs::read(crate::store::sidecar_path(&w.build(), SUFFIX).unwrap()).unwrap(),
            record
        );
        clear_acl(&file);
    }
}
