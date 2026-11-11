// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_emitted_accessors_are_read_off_the_impl_text() {
    assert_eq!(
        public_unsafe_fns(siginfo_impls(Os::Darwin)),
        ["si_addr", "si_pid", "si_uid", "si_status", "si_value"]
    );
    let linux = public_unsafe_fns(siginfo_impls(Os::GnuLinux));
    assert_eq!(
        linux.len(),
        7,
        "{linux:?}: the private `sigchld` is not one"
    );
    assert_eq!(
        public_unsafe_fns("pub unsafe fn (x)\npub  unsafe\tfn a_1 (y) pub unsafe fn b(z)"),
        ["a_1", "b"]
    );
    assert!(public_unsafe_fns("pub fn safe(&self)").is_empty());
}
