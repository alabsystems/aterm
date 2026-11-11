// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_capability_bits_name_the_wake() {
    assert_eq!(wake_of(0x1f), Some(Wake::Full), "a full wake: every bit");
    assert_eq!(
        wake_of(0x0f),
        Some(Wake::Full),
        "measured awake: CPU, graphics, audio, network"
    );
    assert_eq!(
        wake_of(0x03),
        Some(Wake::Full),
        "the CPU and graphics alone"
    );
    assert_eq!(
        wake_of(0x09),
        Some(Wake::Dark),
        "CPU and network: a dark wake"
    );
    assert_eq!(wake_of(0x01), Some(Wake::Dark), "the CPU alone");
    assert_eq!(wake_of(0x00), None, "asleep, or nothing read");
    assert_eq!(wake_of(0x1e), None, "no CPU named");
}

#[cfg(target_os = "macos")]
#[test]
fn a_running_mac_reads_a_wake() {
    let caps = crate::macos::system_capabilities().expect("the root domain's capabilities");
    assert_ne!(
        caps & CAPABILITY_CPU,
        0,
        "a process that runs has the CPU: {caps:#x}"
    );
    assert!(wake().is_some(), "{caps:#x}");
}

#[cfg(not(target_os = "macos"))]
#[test]
fn other_systems_read_none() {
    assert_eq!(wake(), None);
}

/// An assertion this test holds ([`hold`]) is listed under its pid, with its type and name,
/// until it is released; the pid after this one lists none of it.
#[cfg(target_os = "macos")]
#[test]
fn an_assertion_held_is_listed_under_its_pid_until_released() {
    let name = format!("aterm-sysprobe assertion test {}", std::process::id());
    let ours = |list: Vec<Assertion>| -> Vec<String> {
        list.into_iter()
            .filter(|a| a.name.as_deref() == Some(name.as_str()))
            .map(|a| a.kind)
            .collect()
    };
    for kind in ["PreventUserIdleSystemSleep", PREVENT_SYSTEM_SLEEP] {
        let held = hold(kind, &name).expect("the power manager takes it");
        let while_held = ours(assertions_of(std::process::id()).expect("listed"));
        let elsewhere = ours(assertions_of(std::process::id() + 1).expect("listed"));
        drop(held);
        let after = ours(assertions_of(std::process::id()).expect("listed"));
        assert_eq!(while_held, [kind]);
        assert!(elsewhere.is_empty(), "{elsewhere:?}");
        assert!(after.is_empty(), "{kind}: {after:?}");
    }
}

/// A name with a NUL is no assertion, and holds nothing.
#[cfg(target_os = "macos")]
#[test]
fn a_name_with_a_nul_holds_nothing() {
    assert!(hold(PREVENT_SYSTEM_SLEEP, "a\0b").is_none());
}

#[cfg(not(target_os = "macos"))]
#[test]
fn off_macos_no_assertion_is_listed() {
    assert!(assertions_of(1).is_err());
    assert!(hold(PREVENT_SYSTEM_SLEEP, "aterm-sysprobe").is_none());
    assert_eq!(lid_closed(), None);
}
