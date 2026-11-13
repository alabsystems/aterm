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
    assert!(
        !linux.iter().any(|method| method == "sigchld"),
        "private arm helper is not exported"
    );
    for method in linux {
        assert!(
            super::super::behaviour::siginfo_projection(&method).is_some(),
            "every emitted accessor needs independent reference behavior: {method}"
        );
    }
    assert_eq!(
        public_unsafe_fns("pub unsafe fn (x)\npub  unsafe\tfn a_1 (y) pub unsafe fn b(z)"),
        ["a_1", "b"]
    );
    assert!(public_unsafe_fns("pub fn safe(&self)").is_empty());
}
