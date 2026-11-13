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
    let retired_resources = restore.resources_available();
    control.retired().unwrap().finish(&env).unwrap();
    pause.try_wait().expect("actual pending work retired");
    assert_eq!(pending_resources.unwrap(), [false; 4]);
    assert_eq!(retired_resources.unwrap(), [true; 4]);
}
