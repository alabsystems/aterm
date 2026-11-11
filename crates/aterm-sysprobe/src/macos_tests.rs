// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn registry_creator_text_preserves_name_whitespace() {
    let value = Cf::string(c"pid 77, Install Command ").expect("a CFString");
    let text = cf_text_verbatim(value.0).expect("the registry text");
    assert_eq!(
        crate::machine::creator_of(&text),
        Some((77, "Install Command "))
    );
    assert_eq!(cf_text(value.0).as_deref(), Some("pid 77, Install Command"));
}

/// Missing registry properties return NULL and adopt as `None` without
/// calling `CFRelease(NULL)`.
#[test]
fn an_absent_property_adopts_as_none_without_a_release() {
    assert!(Cf::adopt(std::ptr::null()).is_none());
    // SAFETY: A NUL-terminated registry path (the platform expert
    // device, on every Mac and VM); the entry is +1 or 0.
    let device =
        unsafe { iokit::IORegistryEntryFromPath(MAIN_PORT_DEFAULT, c"IOService:/".as_ptr()) };
    assert_ne!(device, 0, "the platform expert device");
    let device = Io(device);
    let present = Cf::string(c"model").expect("a CFString");
    let model = Cf::property(device.0, present.0).and_then(|v| cf_text(v.0));
    assert!(model.is_some(), "a key the entry has reads back");
    let absent = Cf::string(c"aterm-no-such-key").expect("a CFString");
    assert!(Cf::property(device.0, absent.0).is_none());
}

/// A walk over children that never change (the device tree's CPUs) ends
/// with its iterator valid: `IOIteratorIsValid` reads true for a walk that
/// reached its end, so [`gpu_clients`]'s `complete` is not always false.
#[test]
fn a_walk_over_unchanging_children_ends_valid() {
    // SAFETY: A NUL-terminated registry path; the entry is +1 or 0.
    let cpus = unsafe {
        iokit::IORegistryEntryFromPath(MAIN_PORT_DEFAULT, c"IODeviceTree:/cpus".as_ptr())
    };
    assert_ne!(cpus, 0, "the device tree's cpus node");
    let cpus = Io(cpus);
    let mut iter: io_object_t = 0;
    // SAFETY: `cpus` is a live entry, the plane name NUL-terminated,
    // `iter` writable; on success the iterator is +1.
    let kr = unsafe {
        iokit::IORegistryEntryGetChildIterator(cpus.0, c"IODeviceTree".as_ptr(), &raw mut iter)
    };
    assert!(kr == 0 && iter != 0, "{kr}");
    let iter = Io(iter);
    let mut walked = 0;
    // SAFETY: `iter` is a live iterator; each child is +1, released by
    // its `Io`, or 0 at the end.
    while let child @ 1.. = unsafe { iokit::IOIteratorNext(iter.0) } {
        drop(Io(child));
        walked += 1;
    }
    assert!(walked > 0, "a CPU node");
    // SAFETY: `iter` is a live iterator.
    assert_ne!(unsafe { iokit::IOIteratorIsValid(iter.0) }, 0);
}

#[test]
fn open_socket_count_tracks_live_descriptors_and_rejects_invalid_pids() {
    assert_eq!(open_socket_count(0), None);
    let me = std::process::id();
    let before = open_socket_count(me).expect("self socket count");
    let (a, b) = std::os::unix::net::UnixStream::pair().expect("unix socket pair");
    let during = open_socket_count(me).expect("self socket count with pair");
    assert!(
        during >= before + 2,
        "expected at least 2 more sockets ({before} -> {during})"
    );
    drop((a, b));
}
