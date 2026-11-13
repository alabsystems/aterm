// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_scheduled_writer_preserves_the_newest_everyone_key() {
    let lab = crate::client::two_log_macs_for_test(&[(
        crate::text::Audience::Everyone,
        "epoch-probe",
        b"fixture",
    )]);
    let paths = lab.m22.clone();
    let expected = newest(&paths).unwrap().unwrap();
    let held = std::rc::Rc::new(std::cell::RefCell::new(Some(
        crate::client::test_hold_state_lock(&paths).unwrap(),
    )));
    let release = std::rc::Rc::clone(&held);
    crate::client::OBSERVE_BLOCKED.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            release.borrow_mut().take();
        }));
    });
    let observed = newest_started(
        &paths,
        std::time::Instant::now() - std::time::Duration::from_secs(11),
    );
    crate::client::OBSERVE_BLOCKED.with(|hook| hook.borrow_mut().take());
    let observed = observed.unwrap().unwrap();
    assert!(held.borrow().is_none());
    assert_eq!(observed.epoch, expected.epoch);
    assert!(*observed.key == *expected.key);

    let owner = crate::client::test_hold_state_lock(&paths).unwrap();
    for refused in [
        newest_started(
            &paths,
            std::time::Instant::now() - std::time::Duration::from_secs(61),
        ),
        newest_until(&paths, std::time::Instant::now()),
    ] {
        assert_eq!(refused.unwrap_err().kind(), crate::ErrorKind::VaultBusy);
    }
    drop(owner);
    let fresh = newest(&paths).unwrap().unwrap();
    assert_eq!(fresh.epoch, expected.epoch);
    assert!(*fresh.key == *expected.key);
}
