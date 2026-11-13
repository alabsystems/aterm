// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn real_pending_custody_retains_the_original_scope_until_restored_settlement() {
    let _callbacks = PendingCustodyFixture::serial_callbacks().unwrap();
    let lifetime = super::super::super::account_work::Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let (control, env, mut restore) = PendingCustodyFixture::new(&scope).unwrap().into_parts();
    let pending_resources = restore.resources_available();
    drop(scope);
    let pause = lifetime.pause().unwrap();
    assert!(control.has_pending_custody());
    assert!(!control.work_retired());
    assert!(control.retired().is_none());
    control.settle_custody(&env).unwrap();
    assert!(
        control.has_pending_custody(),
        "failed retry restores the actual carrier and scope"
    );
    let pause = pause
        .try_wait()
        .expect_err("the pending carrier owns its original count");
    restore.restore().unwrap();
    control.settle_custody(&env).unwrap();
    assert!(!control.has_pending_custody());
    assert!(control.work_retired());
    let retired_resources = restore.resources_released_within(aterm_spec::wait::MUST_HAPPEN);
    control.retired().unwrap().finish(&env).unwrap();
    pause.try_wait().expect("actual pending work retired");
    assert_eq!(pending_resources.unwrap(), [false; 4]);
    assert_eq!(retired_resources.unwrap(), [true; 4]);
}

/// A released callback port that something outside the custody still listens on (here this
/// test's own listener, as a forked child's copy would) reads held at once and is waited for;
/// one that is never let go is reported held once the budget passes.
#[test]
fn a_released_callback_another_holder_keeps_is_waited_for() {
    let _callbacks = PendingCustodyFixture::serial_callbacks().unwrap();
    let lifetime = super::super::super::account_work::Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let (control, env, mut restore) = PendingCustodyFixture::new(&scope).unwrap().into_parts();
    drop(scope);
    restore.restore().unwrap();
    control.settle_custody(&env).unwrap();
    assert!(control.work_retired());
    // The holder's bind meets the same window the probe waits out.
    let holder = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "a bind of the released callback port",
        || match std::net::TcpListener::bind(restore.callback) {
            Ok(listener) => Some(listener),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => None,
            Err(error) => panic!("bind {}: {error}", restore.callback),
        },
    );
    let held = restore.resources_available();
    let unreleased = restore.resources_released_within(std::time::Duration::ZERO);
    let letting_go = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        drop(holder);
    });
    let released = restore.resources_released_within(aterm_spec::wait::MUST_HAPPEN);
    letting_go.join().unwrap();
    control.retired().unwrap().finish(&env).unwrap();
    assert_eq!(held.unwrap(), [true, true, false, true]);
    assert_eq!(unreleased.unwrap(), [true, true, false, true]);
    assert_eq!(released.unwrap(), [true; 4]);
}
