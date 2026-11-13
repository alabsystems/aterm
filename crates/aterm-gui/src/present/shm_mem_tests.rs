// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_refused_grow_leaves_the_memory_as_it_was() {
    let mut mem = ShmMem::new(c"aterm-shm-test", 4096).expect("memfd");
    mem.words_mut(0, 1024)
        .expect("the mapping")
        .iter_mut()
        .enumerate()
        .for_each(|(i, w)| *w = u32::try_from(i).expect("small"));
    // SAFETY: the test's own memfd; F_SEAL_GROW is a seal it allows.
    let sealed = unsafe { libc::fcntl(mem.fd().as_raw_fd(), libc::F_ADD_SEALS, libc::F_SEAL_GROW) };
    assert_eq!(sealed, 0, "{}", io::Error::last_os_error());
    assert!(mem.set_len(8192).is_err(), "a sealed file grew");
    assert_eq!(mem.len(), 4096);
    let words = mem.words(0, 1024).expect("the old mapping");
    assert!(words.iter().enumerate().all(|(i, &w)| w as usize == i));
    assert!(mem.words(0, 1025).is_none());

    let mut grown = ShmMem::new(c"aterm-shm-test", 4096).expect("memfd");
    grown.words_mut(0, 1).expect("the mapping")[0] = 0x00ab_cdef;
    grown.set_len(8192).expect("grow");
    assert_eq!(grown.len(), 8192);
    assert_eq!(
        grown.words(0, 2048).expect("the new mapping")[0],
        0x00ab_cdef
    );
}
