// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn sweeps_name_both_crates_and_their_subject() {
    let (name, sweep) = &evaluable()[0];
    assert_eq!(*name, "WSTOPSIG");
    let sweep = crates(sweep);
    assert!(sweep.contains(
        "assert!(libc_shim::WSTOPSIG(s) == libc_real::WSTOPSIG(s), \"WSTOPSIG differs\")"
    ));
    assert!(!sweep.contains('@'));
    let si = siginfo("si_pid", siginfo_projection("si_pid").expect("known"));
    assert!(si.contains(
        "assert_eq!(a.si_pid() as i128, b.si_pid() as i128, \"siginfo_t::si_pid differs"
    ));
    assert!(!si.contains('@'));
    assert_eq!(siginfo_projection("unknown_accessor"), None);
}
