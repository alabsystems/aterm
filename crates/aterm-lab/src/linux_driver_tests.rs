// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_arm_is_lima_unless_the_variable_names_vz() {
    assert_eq!(kind_of(None).unwrap(), Kind::Lima);
    assert_eq!(kind_of(Some("")).unwrap(), Kind::Lima);
    assert_eq!(kind_of(Some("lima")).unwrap(), Kind::Lima);
    assert_eq!(kind_of(Some("vz")).unwrap(), Kind::Vz);
    let fail = kind_of(Some("qemu")).unwrap_err();
    assert!(
        fail.msg
            .as_deref()
            .is_some_and(|m| m.starts_with("LAB_LINUX_DRIVER=qemu: lima or vz")),
        "{:?}",
        fail.msg
    );
    assert_eq!(Kind::Vz.name(), "vz");
    assert_eq!(Kind::parse(Kind::Lima.name()), Some(Kind::Lima));
}

#[test]
fn a_verb_that_runs_one_arm_refuses_a_run_that_chose_the_other() {
    let cell = OnceLock::new();
    assert_eq!(choose_in(&cell, "lima").unwrap(), Kind::Lima);
    // The negative control: the same arm named again holds.
    assert_eq!(choose_in(&cell, "lima").unwrap(), Kind::Lima);
    let fail = choose_in(&cell, "vz").unwrap_err();
    assert_eq!(fail.code, 2, "a usage error");
    assert!(
        fail.msg
            .as_deref()
            .is_some_and(|m| m.contains("this run's arm is lima")),
        "{:?}",
        fail.msg
    );
    assert_eq!(cell.get(), Some(&Kind::Lima), "the first arm holds");
    let fresh = OnceLock::new();
    assert_eq!(choose_in(&fresh, "vz").unwrap(), Kind::Vz);
    assert!(choose_in(&fresh, "qemu").is_err());
}

#[test]
fn lima_stops_only_a_vm_this_run_booted_and_did_not_keep() {
    assert!(stops_vm(true, false));
    assert!(!stops_vm(true, true), "--keep-up and up keep it");
    assert!(!stops_vm(false, false), "another run's VM runs on");
    assert!(!stops_vm(false, true));
}
