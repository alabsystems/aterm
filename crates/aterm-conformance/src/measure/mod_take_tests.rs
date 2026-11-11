// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn count_takes_share_custody_and_a_timing_take_excludes_them() {
    let gate = super::RwLock::new(());
    let count = super::take_on(&gate, false);
    assert!(gate.try_read().is_ok());
    assert!(gate.try_write().is_err());
    drop(count);
    let timing = super::take_on(&gate, true);
    assert!(gate.try_read().is_err());
    assert!(gate.try_write().is_err());
    drop(timing);
    assert!(gate.try_write().is_ok());
}
